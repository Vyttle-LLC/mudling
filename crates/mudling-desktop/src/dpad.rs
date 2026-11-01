//! The D-pad: a key for each way a move can go, lit only where the room has
//! an exit. A closed door thickens the key's edge on the side it stands.

use crate::room_panel::{Exit, RoomPanel, button};
use crate::theme::Theme;
use gpui::{Context, Div, Pixels, Stateful, div, prelude::*, px};
use mudling_core::map::Direction;

const KEY_SIZE: Pixels = px(26.);
const KEY_GAP: Pixels = px(2.);

/// The pad's three rows: the compass, with up and down beside it.
const ROWS: [[Option<Direction>; 3]; 3] = [
    [None, Some(Direction::North), Some(Direction::Up)],
    [Some(Direction::West), None, Some(Direction::East)],
    [None, Some(Direction::South), Some(Direction::Down)],
];

fn letter(direction: Direction) -> String {
    direction.letter().to_uppercase()
}

fn door_edge(key: Stateful<Div>, direction: Direction) -> Stateful<Div> {
    match direction {
        Direction::North | Direction::Up => key.border_t_2(),
        Direction::East => key.border_r_2(),
        Direction::South | Direction::Down => key.border_b_2(),
        Direction::West => key.border_l_2(),
    }
}

fn render_lit_key(exit: Exit, theme: &Theme, cx: &mut Context<RoomPanel>) -> Stateful<Div> {
    let word = exit.direction.name();
    let key = button(("dpad", exit.direction as usize), exit.action(), theme)
        .debug_selector(move || format!("dpad-{word}"))
        .size(KEY_SIZE)
        .text_color(theme.accent)
        .on_click(cx.listener(move |panel, event, window, cx| {
            panel.send(exit.command(), event, window, cx);
        }))
        .child(letter(exit.direction));
    if exit.closed {
        door_edge(key, exit.direction).border_color(theme.accent)
    } else {
        key
    }
}

/// Not a button: there's nothing to press, so the keyboard passes over it.
fn render_dark_key(direction: Direction, theme: &Theme) -> Div {
    div()
        .size(KEY_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.placeholder.opacity(0.5))
        .child(letter(direction))
}

pub fn render(exits: &[Exit], theme: &Theme, cx: &mut Context<RoomPanel>) -> impl IntoElement {
    let rows = ROWS.map(|row| {
        let keys = row.map(|direction| {
            let exit = direction
                .and_then(|direction| exits.iter().find(|exit| exit.direction == direction));
            match (exit, direction) {
                (Some(exit), _) => render_lit_key(*exit, theme, cx).into_any_element(),
                (None, Some(direction)) => render_dark_key(direction, theme).into_any_element(),
                (None, None) => div().size(KEY_SIZE).into_any_element(),
            }
        });
        div().flex().gap(KEY_GAP).children(keys)
    });
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(KEY_GAP)
        .text_size(px(11.))
        .children(rows)
}
