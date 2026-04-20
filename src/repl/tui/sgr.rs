//! Shared SGR-state helpers used by both [`super::inline_terminal`]
//! (for cell-by-cell diff emission) and [`super::scrollback`] (for
//! span-by-span history emission). Kept in one place so the mapping
//! from [`Modifier`] bit changes to crossterm `SetAttribute` commands
//! only has to be written once.
//!
//! The transition model tracks *previously* emitted modifiers vs the
//! *desired* modifiers for the next write and produces the minimum
//! set of SGR attribute flips that would get the terminal from one
//! to the other. That's cheaper than blindly resetting + re-setting
//! on every cell / span.

use std::io;

use crossterm::queue;
use crossterm::style::Attribute;
use crossterm::style::SetAttribute;
use ratatui::style::Modifier;

/// A pair of [`Modifier`] sets describing a transition from the
/// currently-emitted SGR attributes (`from`) to the next write's
/// desired attributes (`to`). [`SgrModifierChange::queue`] emits
/// just the SGR command bytes needed to move the terminal from one
/// to the other — e.g. turning bold off and italic on.
pub(super) struct SgrModifierChange {
    pub(super) from: Modifier,
    pub(super) to: Modifier,
}

impl SgrModifierChange {
    pub(super) fn queue<W: io::Write>(self, w: &mut W) -> io::Result<()> {
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(Attribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(Attribute::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(Attribute::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(Attribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(Attribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(Attribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(Attribute::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(Attribute::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(Attribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(Attribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(Attribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(Attribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(Attribute::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(Attribute::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(Attribute::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(Attribute::RapidBlink))?;
        }

        Ok(())
    }
}
