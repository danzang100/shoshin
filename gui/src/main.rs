mod api;
mod app;
mod components;
mod fmt;
mod markdown;
mod md_view;
mod mock;
mod selftest;
mod state;

fn main() {
    console_error_panic_hook::set_once();
    yew::Renderer::<app::App>::new().render();
}
