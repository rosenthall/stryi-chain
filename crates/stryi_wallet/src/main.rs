use cursive::theme::Theme;
use cursive::views::TextView;
use cursive::{Cursive, CursiveExt};

#[tokio::main]
async fn main() {
    let mut siv = Cursive::new();
    siv.set_theme(Theme::terminal_default());

    siv.add_layer(TextView::new("Welcome to Stryi Wallet App"));

    siv.add_global_callback('q', |s| s.quit());

    siv.run();
}
