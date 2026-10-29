//! The window's root view: a centered column with the chat above the command
//! input.

use crate::chat::{COLUMN_WIDTH, Chat, MONO_FONT};
use crate::input::{CommandInput, Submitted};
use crate::theme::Theme;
use gpui::{Context, Entity, Focusable, Subscription, Window, div, prelude::*};

pub struct Shell {
    chat: Entity<Chat>,
    input: Entity<CommandInput>,
    _subscriptions: [Subscription; 2],
}

impl Shell {
    pub fn new(chat: Entity<Chat>, window: &mut Window, cx: &mut Context<Shell>) -> Shell {
        cx.set_global(Theme::for_appearance(window.appearance()));
        let input = cx.new(CommandInput::new);
        let echo = cx.subscribe(&input, |shell, _, Submitted(command), cx| {
            shell
                .chat
                .update(cx, |chat, cx| chat.push_command(command, cx));
        });
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            cx.set_global(Theme::for_appearance(window.appearance()));
            cx.notify();
        });
        window.focus(&input.focus_handle(cx), cx);
        Shell {
            chat,
            input,
            _subscriptions: [echo, appearance],
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Shell>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(MONO_FONT)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .max_w(COLUMN_WIDTH)
                    .px_4()
                    .pt_4()
                    .child(self.chat.clone()),
            )
            .child(
                div()
                    .w_full()
                    .max_w(COLUMN_WIDTH)
                    .p_4()
                    .child(self.input.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, px};
    use mudling_core::replay::ReplayFeed;

    #[gpui::test]
    fn typing_goes_to_the_input_and_return_echoes_it(cx: &mut TestAppContext) {
        cx.update(crate::input::bind_keys);
        let chat = cx.new(|_| {
            let mut chat = Chat::new(1_000);
            chat.load_history(&mut ReplayFeed::new(vec!["a line".into()]), 1_000);
            chat
        });
        let shell_chat = chat.clone();
        let (_, cx) = cx.add_window_view(|window, cx| Shell::new(shell_chat, window, cx));
        chat.update(cx, |chat, cx| chat.scroll_back(px(400.), cx));
        cx.run_until_parked();
        let reading = chat.read_with(cx, |chat, _| chat.scroll_offset());

        cx.simulate_input("look north");
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.scroll_offset()),
            reading,
            "typing must not move the chat"
        );

        cx.simulate_keystrokes("enter");
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.newest_text()).as_deref(),
            Some("look north")
        );
    }
}
