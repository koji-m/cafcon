use std::path::Path;
use std::rc::Rc;
use std::cell::RefCell;
use std::ptr;
use std::env;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use servo;
use servo::gl;
use servo::BrowserId;
use servo::compositing::windowing::{WindowEvent, WindowMethods, MouseWindowEvent};
use servo::compositing::compositor_thread::EventLoopWaker;
use servo::servo_config::resource_files::set_resources_path;
//use servo::servo_config::opts;
use servo::euclid::{
    Point2D, ScaleFactor, Size2D, TypedPoint2D, TypedRect, TypedSize2D, TypedVector2D
};
use servo::ipc_channel::ipc;
use servo::net_traits::net_error_list::NetError;
use servo::script_traits::LoadData;
use servo::servo_geometry::DeviceIndependentPixel;
use servo::servo_url::ServoUrl;
use servo::style_traits::DevicePixel;
use servo::style_traits::cursor::Cursor;
use servo::script_traits::{MouseButton, TouchEventType};
use servo::msg::constellation_msg::{
    Key, KeyModifiers, KeyState, TopLevelBrowsingContextId, TraversalDirection,
};
use epoxy;
use shared_library::dynamic_library::DynamicLibrary;
use glib_itc::{Receiver, Sender, channel};
use gio::{
    self, ActionMapExt, SimpleActionExt, ActionExt,
};
use gtk::{
    self, WidgetExt, WindowExt, GLAreaExt, Inhibit, Continue, ButtonExt,
};
use gdk::{
    self, CONTROL_MASK, ScrollDirection, BUTTON_PRESS_MASK, POINTER_MOTION_MASK,
    SCROLL_MASK, BUTTON_RELEASE_MASK, ScreenExt, WindowExt as _GdkWinExt
};
use gdk::enums::key as gdk_key;
use gdk_sys::{GDK_BUTTON_MIDDLE, GDK_BUTTON_PRIMARY, GDK_BUTTON_SECONDARY};
use hyper::Client;
use hyper::status::StatusCode;
use hyper::client::RedirectPolicy;


const LINE_HEIGHT: f32 = 38.0;

struct Waker {tx: Arc<Mutex<Sender>>}

impl EventLoopWaker for Waker {
    fn clone(&self) -> Box<EventLoopWaker + Send> {
        Box::new(Waker { tx: self.tx.clone() })
    }

    fn wake(&self) {
        self.tx.lock().unwrap().send();
    }
}

pub struct Context {
    pub window: Rc<Window>,
    pub wake_rx: Receiver,
    pub servo: Option<Rc<RefCell<servo::Servo<Window>>>>,
}

pub struct Window {
    pub gl_area: gtk::GLArea,
    pub gtk_window: gtk::ApplicationWindow,
    pub gl: Rc<gl::Gl>,
    pub waker: Box<EventLoopWaker>,
    pub forward_action: Rc<RefCell<gio::SimpleAction>>,
    pub back_action: Rc<RefCell<gio::SimpleAction>>,
    pub pointer: Rc<RefCell<(f64, f64)>>,
    pub chk_req_tx: mpsc::Sender<bool>,
    pub chg_req_rx: Receiver,
    pub event_queue: RefCell<Vec<WindowEvent>>,
}

impl Context {
    pub fn new(app: &gtk::Application, auth_url: &str, test_url: &str) -> Rc<RefCell<Context>> {
        let builder = gtk::Builder::new_from_file(Path::new("./ui/window.ui"));
        let win: gtk::ApplicationWindow = builder.get_object("window").unwrap();
        win.set_application(Some(app));

        let (mut chg_req_tx, mut chg_req_rx) = channel();
        let finish_icon: gtk::Image = builder.get_object("finish-image").unwrap();
        let close_button: gtk::Button = builder.get_object("close-button").unwrap();
        chg_req_rx.connect_recv(move || {
            close_button.set_image(&finish_icon);
            close_button.set_label("Finish");
            Continue(true)
        });

        epoxy::load_with(|s| {
            unsafe {
                match DynamicLibrary::open(None).unwrap().symbol(s) {
                    Ok(v) => v,
                    Err(_) => ptr::null(),
                }
            }
        });

        let gl = unsafe {
            gl::GlFns::load_with(epoxy::get_proc_addr)
        };

        let gl_area: gtk::GLArea = builder.get_object("gl-area").unwrap();
        gl_area.add_events((BUTTON_PRESS_MASK | BUTTON_RELEASE_MASK | POINTER_MOTION_MASK | SCROLL_MASK).bits() as i32);

        let (tx, rx) = channel();
        let (chk_req_tx, chk_req_rx) = mpsc::channel();
        let dummy_back_action = Rc::new(RefCell::new(gio::SimpleAction::new("dummy_back", None)));
        let dummy_forward_action = Rc::new(RefCell::new(gio::SimpleAction::new("dummy_forward", None)));

        let window = Rc::new(Window {
            gl_area: gl_area.clone(),
            gtk_window: win,
            gl: gl,
            waker: Box::new(Waker {tx: Arc::new(Mutex::new(tx))}),
            pointer: Rc::new(RefCell::new((0.0, 0.0))),
            forward_action: dummy_forward_action.clone(),
            back_action: dummy_back_action.clone(),
            chk_req_tx: chk_req_tx,
            chg_req_rx: chg_req_rx,
            event_queue: RefCell::new(vec![]),
        });

        let context = Rc::new(RefCell::new(Context {
            window: window.clone(),
            wake_rx: rx,
            servo: None,
        }));

        {
            let context = context.clone();
            let auth_url = auth_url.to_string();
            gl_area.connect_realize(move |_| {
                init_servo(context.clone(), &auth_url);
            });
        }


        let mut checker = Client::new();
        checker.set_redirect_policy(RedirectPolicy::FollowNone);
        let test_url = test_url.to_string();
        thread::spawn(move || {
            while let Ok(true) = chk_req_rx.recv() {
                println!("DEBUG: communication ack");
                if let Ok(res) = checker.head(&test_url).send() {
                    if res.status == StatusCode::Ok {
                        chg_req_tx.send();
                    }
                }
            }
        });

        context
    }
}

impl Window {
    pub fn maybe_change_close_button(&self) {
        self.chk_req_tx.send(true).unwrap();
    }
}

impl WindowMethods for Window {
    fn prepare_for_composite(&self, _width: usize, _height: usize) -> bool {
        self.gl_area.make_current();
        true
    }

    fn present(&self) {
        self.gl_area.queue_render();
    }

    fn supports_clipboard(&self) -> bool {
        true
    }

    fn create_event_loop_waker(&self) -> Box<EventLoopWaker> {
        self.waker.clone()
    }

    fn gl(&self) -> Rc<gl::Gl> {
        self.gl.clone()
    }

    fn hidpi_factor(&self) -> ScaleFactor<f32, DeviceIndependentPixel, DevicePixel> {
        ScaleFactor::new(self.gl_area.get_scale_factor() as f32)
    }

    fn framebuffer_size(&self) -> TypedSize2D<u32, DevicePixel> {
        let gtk::Allocation {width, height, ..} = self.gl_area.get_allocation();
        let factor = self.gl_area.get_scale_factor() as u32;
        TypedSize2D::new(factor * width as u32, factor * height as u32)
    }

    fn window_rect(&self) -> TypedRect<u32, DevicePixel> {
        TypedRect::new(TypedPoint2D::new(0, 0), self.framebuffer_size())
    }

    fn size(&self) -> TypedSize2D<f32, DeviceIndependentPixel> {
        let gtk::Allocation {width, height, ..} = self.gl_area.get_allocation();
        TypedSize2D::new(width as f32, height as f32)
    }

    fn client_window(&self, _id: BrowserId) -> (Size2D<u32>, Point2D<i32>) {
        let gtk::Allocation {x, y, width, height} = self.gl_area.get_allocation();
        (Size2D::new(width as u32, height as u32), Point2D::new(x, y))
    }

    fn screen_size(&self, _: TopLevelBrowsingContextId) -> Size2D<u32> {
        let screen = self.gtk_window.get_screen().unwrap();
        let monitor = screen.get_monitor_at_window(&screen.get_active_window().unwrap());
        let geometry = screen.get_monitor_geometry(monitor);
        Size2D::new(geometry.width as u32, geometry.height as u32)
    }

    fn screen_avail_size(&self, _: TopLevelBrowsingContextId) -> Size2D<u32> {
        let screen = self.gtk_window.get_screen().unwrap();
        let monitor = screen.get_monitor_at_window(&screen.get_active_window().unwrap());
        let geometry = screen.get_monitor_geometry(monitor);
        Size2D::new(geometry.width as u32, geometry.height as u32)
    }

    fn set_page_title(&self, _id: BrowserId, title: Option<String>) {
        self.gtk_window.set_title(match title {
            Some(ref title) => title,
            None => "",
        });
    }

    fn allow_navigation(&self, _id: BrowserId, _url: ServoUrl, chan: ipc::IpcSender<bool>) {
        chan.send(true).ok();
    }

    fn set_inner_size(&self, _id: BrowserId, _size: Size2D<u32>) {
    }

    fn set_position(&self, _id: BrowserId, _point: Point2D<i32>) {
    }

    fn set_fullscreen_state(&self, _id: BrowserId, _state: bool) {
    }

    fn status(&self, _id: BrowserId, _status: Option<String>) {
    }

    fn load_start(&self, _id: BrowserId) {
        println!("load_start");
    }

    fn load_end(&self, _id: BrowserId) {
        self.maybe_change_close_button();
        println!("load_end");
    }

    fn load_error(&self, _id: BrowserId, _: NetError, _url: String) {
    }

    fn head_parsed(&self, _id: BrowserId) {
        println!("head_parsed");
    }

    fn history_changed(&self, _id: BrowserId, entries: Vec<LoadData>, current: usize) {
        println!("history_changed");
        //TODO: should bind LoadData with back/forward action
        self.back_action.borrow().set_enabled(!entries.is_empty() && current > 0);
        self.forward_action.borrow().set_enabled(!entries.is_empty() && current < entries.len() - 1);
    }

    fn set_cursor(&self, cursor: Cursor) {
        let name = match cursor {
            Cursor::None => "none",
            Cursor::Default => "default",
            Cursor::Help => "help",
            Cursor::Pointer => "pointer",
            Cursor::ContextMenu => "context-menu",
            Cursor::Progress => "progress",
            Cursor::Wait => "wait",
            Cursor::Cell => "cell",
            Cursor::Crosshair => "crosshair",
            Cursor::Text => "text",
            Cursor::VerticalText => "vertical-text",
            Cursor::Alias => "alias",
            Cursor::Copy => "copy",
            Cursor::NoDrop => "no-drop",
            Cursor::Move => "move",
            Cursor::NotAllowed => "not-allowed",
            Cursor::Grab => "grab",
            Cursor::Grabbing => "grabbing",
            Cursor::AllScroll => "all-scroll",
            Cursor::ColResize => "col-resize",
            Cursor::RowResize => "row-resize",
            Cursor::NResize => "n-resize",
            Cursor::EResize => "e-resize",
            Cursor::SResize => "s-resize",
            Cursor::WResize => "w-resize",
            Cursor::NeResize => "ne-resize",
            Cursor::NwResize => "nw-resize",
            Cursor::SwResize => "sw-resize",
            Cursor::SeResize => "se-resize",
            Cursor::EwResize => "ew-resize",
            Cursor::NsResize => "ns-resize",
            Cursor::NeswResize => "nesw-resize",
            Cursor::NwseResize => "nwse-resize",
            Cursor::ZoomIn => "zoom-in",
            Cursor::ZoomOut => "zoom-out",
        };
        let display = gdk::Display::get_default().unwrap();
        let cursor = gdk::Cursor::new_from_name(&display, name);
        let window = self.gl_area.get_window().unwrap();
        window.set_cursor(&cursor);
    }

    fn set_favicon(&self, _id: BrowserId, _url: ServoUrl) {
    }

    fn handle_key(&self, _id: Option<BrowserId>, ch: Option<char>, key: Key, mods: KeyModifiers) {
        println!("handle_key");
        match (key, ch, mods) {
            (Key::Down, None, KeyModifiers::NONE) => {
                let delta = servo::webrender_api::ScrollLocation::Delta(TypedVector2D::new(0.0, -LINE_HEIGHT * 2.0));
                let (x, y) = *self.pointer.borrow();
                let origin = TypedPoint2D::new(x as i32, y as i32);
                self.event_queue.borrow_mut().push(WindowEvent::Scroll(delta, origin, TouchEventType::Down));
            },
            (Key::Up, None, KeyModifiers::NONE) => {
                let delta = servo::webrender_api::ScrollLocation::Delta(TypedVector2D::new(0.0, LINE_HEIGHT * 2.0));
                let (x, y) = *self.pointer.borrow();
                let origin = TypedPoint2D::new(x as i32, y as i32);
                self.event_queue.borrow_mut().push(WindowEvent::Scroll(delta, origin, TouchEventType::Up));
            },
            _ => {
            }
        }
    }
}

fn init_servo(context: Rc<RefCell<Context>>, url: &str) {
    context.borrow().window.gl_area.make_current();

    let servo = Rc::new(RefCell::new(servo::Servo::new(context.borrow().window.clone())));

    //connect events to gl_area
    {
        let servo = servo.clone();
        let ctx = context.clone();
        context.borrow_mut().wake_rx.connect_recv(move || {
            servo.borrow_mut().handle_events(vec![]);
            let ctx = ctx.borrow();
            let event_queue = &mut *ctx.window.event_queue.borrow_mut();
            if let Some(ev) = event_queue.pop() {
                servo.borrow_mut().handle_events(vec![ev]);
            }
            Continue(true)
        });
    }


    {
        let servo = servo.clone();
        context.borrow().window.gl_area.connect_key_press_event(move |_, event| {
            println!("key pressed");
            let (ch, key) = to_key(event.get_keyval());
            println!("key: {:?}, {:?}", ch, key);
            if let Some(key) = key {
                let modifier = to_modifier(event.get_state());
                servo.borrow_mut().handle_events(
                    vec![WindowEvent::KeyEvent(ch, key, KeyState::Pressed, modifier)]);
            }
            Inhibit(true)
        });
    }

    {
        let servo = servo.clone();
        context.borrow().window.gl_area.connect_key_release_event(move |_, event| {
            println!("key released");
            let (ch, key) = to_key(event.get_keyval());
            println!("key: {:?}, {:?}", ch, key);
            if let Some(key) = key {
                let modifier = to_modifier(event.get_state());
                servo.borrow_mut().handle_events(
                    vec![WindowEvent::KeyEvent(ch, key, KeyState::Released, modifier)]);
            }
            Inhibit(true)
        });
    }

    {
        let servo = servo.clone();
        context.borrow().window.gl_area.connect_button_press_event(move |_, event| {
            let (x, y) = event.get_position();
            let mouse_ev = MouseWindowEvent::MouseDown(to_mouse_button(event.get_button()),
                                                       TypedPoint2D::new(x as f32, y as f32));
            servo.borrow_mut().handle_events(vec![WindowEvent::MouseWindowEventClass(mouse_ev)]);
            Inhibit(false)
        });
    }

    {
        let servo = servo.clone();
        context.borrow().window.gl_area.connect_button_release_event(move |_, event| {
            let (x, y) = event.get_position();
            let button = to_mouse_button(event.get_button());
            let mouseup_ev = WindowEvent::MouseWindowEventClass(
                MouseWindowEvent::MouseUp(button,
                                          TypedPoint2D::new(x as f32, y as f32)));
            let click_ev = WindowEvent::MouseWindowEventClass(
                MouseWindowEvent::Click(button,
                                        TypedPoint2D::new(x as f32, y as f32)));
            servo.borrow_mut().handle_events(vec![mouseup_ev, click_ev]);
            Inhibit(false)
        });
    }

    {
        let servo = servo.clone();
        let ctx = context.clone();
        context.borrow().window.gl_area.connect_motion_notify_event(move |_, event| {
            let (x, y) = event.get_position();
            let ctx = ctx.borrow();
            *ctx.window.pointer.borrow_mut() = (x, y);
            let ev = WindowEvent::MouseWindowMoveEventClass(TypedPoint2D::new(x as f32, y as f32));
            servo.borrow_mut().handle_events(vec![ev]);
            Inhibit(false)
        });
    }

    {
        let servo = servo.clone();
        context.borrow().window.gl_area.connect_resize(move |_, _, _| {
            servo.borrow_mut().handle_events(vec![WindowEvent::Resize]);
            servo.borrow_mut().handle_events(vec![WindowEvent::Refresh]);
        });
    }

    {
        let servo = servo.clone();
        let ctx= context.clone();
        context.borrow().window.gl_area.connect_scroll_event(move |_, event| {
            if !event.get_state().contains(CONTROL_MASK) {
                let ev_type = match event.get_direction() {
                    ScrollDirection::Down => TouchEventType::Down,
                    ScrollDirection::Up => TouchEventType::Up,
                    ScrollDirection::Left => TouchEventType::Cancel, //TODO
                    ScrollDirection::Right => TouchEventType::Cancel, //TODO
                    _ => TouchEventType::Cancel,
                };

                let dx: f32 = 0.0;
                let dy: f32 = match ev_type {
                    TouchEventType::Down => -LINE_HEIGHT,
                    TouchEventType::Up => LINE_HEIGHT,
                    _ => 0.0,
                };

                let delta = servo::webrender_api::ScrollLocation::Delta(TypedVector2D::new(dx, dy));
                let ctx = ctx.borrow();
                let (x, y) = *ctx.window.pointer.borrow();
                let origin = TypedPoint2D::new(x as i32, y as i32);
                servo.borrow_mut().handle_events(vec![WindowEvent::Scroll(delta, origin, ev_type)]);
            }
            Inhibit(false)
        });
    }

    let path = env::current_dir().unwrap().join("resources");
    let path = path.to_str().unwrap().to_string();
    set_resources_path(Some(path));

    let url = ServoUrl::parse(url).unwrap();
    let (sender, receiver) = ipc::channel().unwrap();
    servo.borrow_mut().handle_events(vec![WindowEvent::NewBrowser(url, sender)]);
    let browser_id = receiver.recv().unwrap();
    servo.borrow_mut().handle_events(vec![WindowEvent::SelectBrowser(browser_id)]);

    //TODO: should be stateful by new_stateful
    let back_action = gio::SimpleAction::new("back-history", None);
    let forward_action = gio::SimpleAction::new("forward-history", None);

    {
        let servo = servo.clone();
        back_action.connect_activate(move |_, _| {
            println!("back action");
            //TODO: should load page using LoadData from action state
            let event = WindowEvent::Navigation(browser_id, TraversalDirection::Back(1));
            servo.borrow_mut().handle_events(vec![event]);
        });
    }

    {
        let servo = servo.clone();
        forward_action.connect_activate(move |_, _| {
            //TODO: should load page using LoadData from action state
            let event = WindowEvent::Navigation(browser_id, TraversalDirection::Forward(1));
            servo.borrow_mut().handle_events(vec![event]);
        });
    }

    context.borrow().window.gtk_window.add_action(&back_action);
    context.borrow().window.gtk_window.add_action(&forward_action);

    {
        let context = context.borrow();
        *context.window.back_action.borrow_mut() = back_action;
        *context.window.forward_action.borrow_mut() = forward_action;
    }

    context.borrow_mut().servo = Some(servo);
}

//helpers
fn to_mouse_button(gdk_button: u32) -> MouseButton {
    match gdk_button as i32 {
        GDK_BUTTON_PRIMARY => MouseButton::Left,
        GDK_BUTTON_SECONDARY => MouseButton::Right,
        GDK_BUTTON_MIDDLE => MouseButton::Middle,
        _ => panic!("GDK mouse button click error"),
    }
}

fn to_key(gdk_key: gdk_key::Key) -> (Option<char>, Option<Key>) {
    let unicode =
        gdk::keyval_to_unicode(gdk_key).and_then(|ch| {
            if ch.is_control() {
                None
            } else {
                Some(ch)
            }
        });
    let key = match gdk_key {
        gdk_key::space => Key::Space,
        gdk_key::apostrophe => Key::Apostrophe,
        gdk_key::comma => Key::Comma,
        gdk_key::minus => Key::Minus,
        gdk_key::period => Key::Period,
        gdk_key::slash => Key::Slash,
        gdk_key::_0 => Key::Num0,
        gdk_key::_1 => Key::Num1,
        gdk_key::_2 => Key::Num2,
        gdk_key::_3 => Key::Num3,
        gdk_key::_4 => Key::Num4,
        gdk_key::_5 => Key::Num5,
        gdk_key::_6 => Key::Num6,
        gdk_key::_7 => Key::Num7,
        gdk_key::_8 => Key::Num8,
        gdk_key::_9 => Key::Num9,
        gdk_key::semicolon => Key::Semicolon,
        gdk_key::equal => Key::Equal,
        gdk_key::A | gdk_key::a => Key::A,
        gdk_key::B | gdk_key::b => Key::B,
        gdk_key::C | gdk_key::c => Key::C,
        gdk_key::D | gdk_key::d => Key::D,
        gdk_key::E | gdk_key::e => Key::E,
        gdk_key::F | gdk_key::f => Key::F,
        gdk_key::G | gdk_key::g => Key::G,
        gdk_key::H | gdk_key::h => Key::H,
        gdk_key::I | gdk_key::i => Key::I,
        gdk_key::J | gdk_key::j => Key::J,
        gdk_key::K | gdk_key::k => Key::K,
        gdk_key::L | gdk_key::l => Key::L,
        gdk_key::M | gdk_key::m => Key::M,
        gdk_key::N | gdk_key::n => Key::N,
        gdk_key::O | gdk_key::o => Key::O,
        gdk_key::P | gdk_key::p => Key::P,
        gdk_key::Q | gdk_key::q => Key::Q,
        gdk_key::R | gdk_key::r => Key::R,
        gdk_key::S | gdk_key::s => Key::S,
        gdk_key::T | gdk_key::t => Key::T,
        gdk_key::U | gdk_key::u => Key::U,
        gdk_key::V | gdk_key::v => Key::V,
        gdk_key::W | gdk_key::w => Key::W,
        gdk_key::X | gdk_key::x => Key::X,
        gdk_key::Y | gdk_key::y => Key::Y,
        gdk_key::Z | gdk_key::z => Key::Z,
        gdk_key::bracketleft => Key::LeftBracket,
        gdk_key::backslash => Key::Backslash,
        gdk_key::bracketright => Key::RightBracket,
        gdk_key::dead_grave => Key::GraveAccent,
        gdk_key::Escape => Key::Escape,
        gdk_key::Return => Key::Enter,
        gdk_key::Tab => Key::Tab,
        gdk_key::BackSpace => Key::Backspace,
        gdk_key::Insert => Key::Insert,
        gdk_key::Delete => Key::Delete,
        gdk_key::Right => Key::Right,
        gdk_key::Left => Key::Left,
        gdk_key::Down => Key::Down,
        gdk_key::Up => Key::Up,
        gdk_key::Page_Up => Key::PageUp,
        gdk_key::Page_Down => Key::PageDown,
        gdk_key::Home => Key::Home,
        gdk_key::End => Key::End,
        gdk_key::Caps_Lock => Key::CapsLock,
        gdk_key::Scroll_Lock => Key::ScrollLock,
        gdk_key::Num_Lock => Key::NumLock,
        gdk_key::_3270_PrintScreen => Key::PrintScreen, // TODO
        gdk_key::Pause => Key::Pause,
        gdk_key::F1 => Key::F1,
        gdk_key::F2 => Key::F2,
        gdk_key::F3 => Key::F3,
        gdk_key::F4 => Key::F4,
        gdk_key::F5 => Key::F5,
        gdk_key::F6 => Key::F6,
        gdk_key::F7 => Key::F7,
        gdk_key::F8 => Key::F8,
        gdk_key::F9 => Key::F9,
        gdk_key::F10 => Key::F10,
        gdk_key::F11 => Key::F11,
        gdk_key::F12 => Key::F12,
        gdk_key::F13 => Key::F13,
        gdk_key::F14 => Key::F14,
        gdk_key::F15 => Key::F15,
        gdk_key::F16 => Key::F16,
        gdk_key::F17 => Key::F17,
        gdk_key::F18 => Key::F18,
        gdk_key::F19 => Key::F19,
        gdk_key::F20 => Key::F20,
        gdk_key::F21 => Key::F21,
        gdk_key::F22 => Key::F22,
        gdk_key::F23 => Key::F23,
        gdk_key::F24 => Key::F24,
        gdk_key::F25 => Key::F25,
        gdk_key::KP_0 => Key::Kp0,
        gdk_key::KP_1 => Key::Kp1,
        gdk_key::KP_2 => Key::Kp2,
        gdk_key::KP_3 => Key::Kp3,
        gdk_key::KP_4 => Key::Kp4,
        gdk_key::KP_5 => Key::Kp5,
        gdk_key::KP_6 => Key::Kp6,
        gdk_key::KP_7 => Key::Kp7,
        gdk_key::KP_8 => Key::Kp8,
        gdk_key::KP_9 => Key::Kp9,
        gdk_key::KP_Decimal => Key::KpDecimal,
        gdk_key::KP_Divide => Key::KpDivide,
        gdk_key::KP_Multiply => Key::KpMultiply,
        gdk_key::KP_Subtract => Key::KpSubtract,
        gdk_key::KP_Add => Key::KpAdd,
        gdk_key::KP_Enter => Key::KpEnter,
        gdk_key::KP_Equal => Key::KpEqual,
        gdk_key::Shift_L => Key::LeftShift,
        gdk_key::Control_L => Key::LeftControl,
        gdk_key::Alt_L => Key::LeftAlt,
        gdk_key::Super_L => Key::LeftSuper,
        gdk_key::Shift_R => Key::RightShift,
        gdk_key::Control_R => Key::RightControl,
        gdk_key::Alt_R => Key::RightAlt,
        gdk_key::Super_R => Key::RightSuper,
        gdk_key::Menu => Key::Menu,
        //gdk_key:: => Key::World1, // TODO
        //gdk_key:: => Key::World2, // TODO
        //gdk_key:: => Key::NavigateBackward, // TODO
        //gdk_key:: => Key::NavigateForward, // TODO
        _ => return (None, None)
    };
    (unicode, Some(key))
}

fn to_modifier(mods: gdk::ModifierType) -> KeyModifiers {
    let mut key_mods = KeyModifiers::empty();
    if mods.contains(gdk::META_MASK) {
        key_mods.insert(KeyModifiers::ALT);
    }
    if mods.contains(gdk::SUPER_MASK) {
        key_mods.insert(KeyModifiers::SUPER);
    }
    if mods.contains(gdk::CONTROL_MASK) {
        key_mods.insert(KeyModifiers::CONTROL);
    }
    if mods.contains(gdk::SHIFT_MASK) {
        key_mods.insert(KeyModifiers::SHIFT);
    }
    key_mods
}

