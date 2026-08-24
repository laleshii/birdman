use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, Context, Window};

use crate::config;
use crate::theme;

pub struct Onboarding {
    pub config_path: PathBuf,
    pub error: Option<String>,
}

impl Render for Onboarding {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path_str = display_path(&self.config_path);

        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_6()
            .w(px(460.0))
            .rounded_lg()
            .bg(theme::color(theme::BG_SIDEBAR))
            .border_1()
            .border_color(theme::color(theme::BORDER))
            .child(
                div()
                    .text_size(px(16.0))
                    .text_color(theme::color(theme::TEXT_PRIMARY))
                    .child("No account configured"),
            );

        if let Some(error) = &self.error {
            card = card.child(
                div()
                    .text_color(theme::color(theme::DANGER))
                    .text_size(px(12.0))
                    .child(format!("Config file problem: {error}")),
            );
        }

        card = card
            .child(
                div()
                    .text_color(theme::color(theme::TEXT_SECONDARY))
                    .text_size(px(12.0))
                    .child(format!(
                        "Add an [account] section to {path_str} (a commented example is already \
                         in there), then restart Birdman to connect."
                    )),
            )
            .child(
                // Wrapped rather than `self_start()`, which this gpui does not
                // have: the row is what stops the button stretching to the
                // column's full width.
                div().flex().child(
                    div()
                        .id("open-config")
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(theme::color(theme::BG_HOVER))
                        .hover(|el| el.bg(theme::color(theme::BG_SELECTED)))
                        .cursor_pointer()
                        .text_color(theme::color(theme::TEXT_PRIMARY))
                        .child("Open config file")
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Off the UI thread: even the spawn call must not block it.
                            let path = this.config_path.clone();
                            let open_task =
                                cx.background_spawn(async move { config::open_editor(&path) });
                            cx.spawn(async move |this, cx| {
                                if let Err(err) = open_task.await {
                                    let _ = this.update(cx, |this, cx| {
                                        this.error = Some(format!("couldn't open editor: {err}"));
                                        cx.notify();
                                    });
                                }
                            })
                            .detach();
                        })),
                ),
            );

        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(theme::color(theme::BG_APP))
            .font_family("Liberation Sans")
            .child(card)
    }
}

fn display_path(path: &Path) -> String {
    match dirs::home_dir() {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}
