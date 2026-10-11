mod api;
mod app;
mod components;
mod fmt;
mod mock;
mod state;

fn main() {
    console_error_panic_hook::set_once();
    yew::Renderer::<app::App>::new().render();
}
