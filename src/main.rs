extern crate gtk;
extern crate gio;
extern crate gdk;
extern crate gdk_sys;
extern crate servo;
extern crate epoxy;
extern crate shared_library;
extern crate glib_itc;
extern crate hyper;

use std::env::Args;

use gio::{
    ApplicationExt, ApplicationExtManual, SimpleActionExt, ActionMapExt,
    FileExt,
};

use gtk::{
    WidgetExt, GtkApplicationExt,
};
use hyper::Client;
use hyper::status::StatusCode;
use hyper::header::Location;
use hyper::client::RedirectPolicy;

mod window;
use window::Context;

fn init_actions(app: &gtk::Application) {
    let quit_action = gio::SimpleAction::new("quit", None);
    {
        let app = app.clone();
        quit_action.connect_activate(move |_, _| {
            app.quit();
        });
    }

    app.add_action(&quit_action);
}

fn init_accels(app: &gtk::Application) {
    app.add_accelerator("Escape", "app.quit", None);
}

fn run(args: Args) {
    match gtk::Application::new("com.github.koji-m.cafe_auth", gio::APPLICATION_HANDLES_OPEN) {
        Ok(app) => {
            {
                app.connect_startup(move |app| {
                    init_actions(app);
                    init_accels(app);
                });
            }

            {
                app.connect_activate(move |app| {
                    let ctx = Context::new(app, "http://www.google.com", "http://www.google.com");
                    let win = ctx.borrow().window.gtk_window.clone();
                    win.show_all();
                });
            }

            {
                app.connect_open(move |app, urls, _| {
                    if let Some(test_url) = urls[0].get_uri() {
                        if let Some(auth_url) = check_auth_url(&test_url) {
                            let ctx = Context::new(app, &auth_url, &test_url);
                            let win = ctx.borrow().window.gtk_window.clone();
                            win.show_all();
                        }
                    }
                });
            }


            let args: Vec<String> = args.collect();
            let argv: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();

            app.run(argv.as_slice());
        },

        Err(_) => {
            println!("Application startup error");
        }
    };
}

fn check_auth_url(test_url: &str) -> Option<String> {
    let mut checker = Client::new();
    checker.set_redirect_policy(RedirectPolicy::FollowNone);
    if let Ok(res) = checker.head(test_url).send() {
        if res.status == StatusCode::Found {
            if let Some(url) = res.headers.get::<Location>() {
                println!("redirect: {}", &url);
                return Some(String::from(url.as_str()));
            }
            println!("no location field");
        }
        println!("status code {:?}", res.status);
    }
    println!("http head failed");
    return None;
}

fn main() {
    run(std::env::args());
}

