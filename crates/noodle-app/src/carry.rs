//! Carrying a clip or a track out of the timeline and dropping it somewhere
//! else: into the node graph, or onto a Sampler.
//!
//! The timeline starts the gesture, since a drag of a clip or a track header
//! begins there. While the pointer is outside the timeline the item is
//! *carried*: a label follows the pointer, and the timeline leaves the clip
//! where it was. Letting go outside the timeline leaves a [`Dropped`] in the
//! egui context for whichever view is under the pointer to claim with
//! [`take_drop`]. The views are drawn in different orders, so a drop stays for
//! one more frame than the one it happened in; one nobody claims is forgotten.

use egui::{Align2, Color32, Context, FontId, Id, Order, Pos2, Rect, Vec2};
use noodle_core::{ClipId, NodeId};

/// Something carried out of the timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Clip(ClipId),
    /// A track, by its group.
    Track(NodeId),
}

/// An item let go of at `at` (screen points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dropped {
    pub item: Item,
    pub at: Pos2,
    frame: u64,
}

fn carrying_id() -> Id {
    Id::new("carried item")
}

fn drop_id() -> Id {
    Id::new("dropped item")
}

/// Marks `item` as carried this frame, with the pointer at `at`, and draws a
/// label by the pointer. Call it every frame the item is carried.
pub fn carrying(ctx: &Context, item: Item, at: Pos2, label: &str) {
    ctx.data_mut(|d| d.insert_temp(carrying_id(), Some((item, at))));
    let painter = ctx.layer_painter(egui::LayerId::new(Order::Tooltip, Id::new("carry label")));
    let text = painter.text(
        at + Vec2::new(14.0, 14.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(13.0),
        Color32::WHITE,
    );
    painter.rect_filled(
        text.expand2(Vec2::new(6.0, 3.0)),
        4.0,
        Color32::from_black_alpha(200),
    );
    painter.text(
        at + Vec2::new(14.0, 14.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(13.0),
        Color32::WHITE,
    );
    ctx.request_repaint();
}

/// What is being carried, and where the pointer is, if anything is.
pub fn carried(ctx: &Context) -> Option<(Item, Pos2)> {
    ctx.data(|d| d.get_temp::<Option<(Item, Pos2)>>(carrying_id()))
        .flatten()
}

/// Ends the carry. Call it when the pointer comes back into the timeline.
pub fn cancel(ctx: &Context) {
    ctx.data_mut(|d| d.insert_temp(carrying_id(), None::<(Item, Pos2)>));
}

/// Lets go of the carried item at `at`: for a view to claim.
pub fn release(ctx: &Context, item: Item, at: Pos2) {
    cancel(ctx);
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| d.insert_temp(drop_id(), Some(Dropped { item, at, frame })));
    ctx.request_repaint();
}

/// The drop waiting, if the pointer was inside `area` when it happened. It is
/// not claimed until [`take_drop`].
pub fn drop_in(ctx: &Context, area: Rect) -> Option<Dropped> {
    ctx.data(|d| d.get_temp::<Option<Dropped>>(drop_id()))
        .flatten()
        .filter(|drop| area.contains(drop.at))
}

/// Claims the waiting drop.
pub fn take_drop(ctx: &Context) {
    ctx.data_mut(|d| d.insert_temp(drop_id(), None::<Dropped>));
}

/// Forgets a drop nobody has claimed in the frame after it happened. Call it
/// once at the end of every frame.
pub fn expire(ctx: &Context) {
    let now = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| {
        if d.get_temp::<Option<Dropped>>(drop_id())
            .flatten()
            .is_some_and(|drop| now > drop.frame)
        {
            d.insert_temp(drop_id(), None::<Dropped>);
        }
    });
}
