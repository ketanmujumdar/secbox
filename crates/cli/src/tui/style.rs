//! The colours of the interface, from the sixteen every terminal has.
//!
//! Each style is a foreground and background pair, written as the escape
//! codes a terminal understands. Keeping them in one place means the layout
//! never writes an escape sequence itself, so a width calculation cannot
//! count one by mistake. The look is a dark page with a bar along the top and
//! the bottom, single-ruled boxes and one accent colour.

/// A colour pair. The names are roles, not colours, so the palette can change
/// in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// The bar along the top, with the program's name.
    MenuBar,
    /// The warning set at the right of the top bar.
    MenuAlert,
    /// The page everything else sits on.
    Window,
    WindowDim,
    WindowGood,
    WindowWarn,
    WindowAlert,
    /// What can be chosen, such as New project, or the box being chosen.
    Accent,
    Frame,
    /// The frame of a box that needs to be noticed, such as one counting
    /// refused lookups.
    FrameAlert,
    Title,
    /// The highlighted row of a list.
    Selected,
    /// The tab showing, and the key of the choice chosen.
    TabActive,
    /// The letter that opens a tab, or any key named on the page.
    TabKey,
    /// A line being typed into.
    Field,
    /// What a line shows before anything is typed.
    FieldDim,
    /// The key in front of a choice that is not the one chosen.
    Badge,
    /// A question or a report over everything else, on a lighter grey.
    Dialog,
    DialogDim,
    DialogHot,
    DialogAlert,
    DialogFrame,
    /// The answer Enter gives, which is always the one that changes nothing.
    Button,
    ButtonHot,
    /// The bar along the bottom: the key, then the word.
    StatusKey,
    StatusLabel,
    StatusGood,
    StatusAlert,
    Scrollbar,
    ScrollThumb,
}

impl Style {
    /// The escape sequence that turns this style on.
    pub fn codes(self) -> &'static str {
        match self {
            Style::MenuBar => "97;100",
            Style::MenuAlert => "97;41",
            Style::Window => "37;40",
            Style::WindowDim => "90;40",
            Style::WindowGood => "92;40",
            Style::WindowWarn => "93;40",
            Style::WindowAlert => "91;40",
            Style::Accent => "96;40",
            Style::Frame => "90;40",
            Style::FrameAlert => "91;40",
            Style::Title => "97;40",
            Style::Selected => "97;44",
            Style::TabActive => "30;46",
            Style::TabKey => "93;40",
            Style::Field => "97;100",
            Style::FieldDim => "37;100",
            Style::Badge => "97;100",
            Style::Dialog => "97;100",
            Style::DialogDim => "37;100",
            Style::DialogHot => "93;100",
            Style::DialogAlert => "91;100",
            Style::DialogFrame => "96;100",
            Style::Button => "30;46",
            Style::ButtonHot => "31;46",
            Style::StatusKey => "93;100",
            Style::StatusLabel => "37;100",
            Style::StatusGood => "92;100",
            Style::StatusAlert => "91;100",
            Style::Scrollbar => "90;40",
            Style::ScrollThumb => "37;40",
        }
    }
}

/// The same colour from the fixed part of the 256-colour table. Terminal themes
/// recolour the first sixteen entries, and leave these alone.
fn fixed_index(code: u16) -> Option<u16> {
    Some(match code {
        30 => 16,
        31 => 124,
        32 => 34,
        33 => 142,
        34 => 19,
        35 => 127,
        36 => 37,
        37 => 250,
        90 => 244,
        91 => 196,
        92 => 46,
        93 => 226,
        94 => 21,
        95 => 201,
        96 => 51,
        97 => 231,
        _ => return None,
    })
}

/// As [`fixed_index`], except that the bright black behind the bars and the
/// boxes is a dark grey, so that white on it stays easy to read.
fn fixed_background(code: u16) -> Option<u16> {
    match code {
        90 => Some(236),
        other => fixed_index(other),
    }
}

impl Style {
    /// The escape parameters for this style, on a terminal that does or does
    /// not have the 256-colour table.
    pub fn sequence(self, fixed: bool) -> String {
        let codes = self.codes();
        if !fixed {
            return codes.to_owned();
        }
        let converted = codes.split_once(';').and_then(|(fg, bg)| {
            let fg = fixed_index(fg.parse().ok()?)?;
            let bg = fixed_background(bg.parse::<u16>().ok()?.checked_sub(10)?)?;
            Some(format!("38;5;{fg};48;5;{bg}"))
        });
        converted.unwrap_or_else(|| codes.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::Style;

    #[test]
    fn the_page_is_the_same_colour_whatever_the_theme_says() {
        assert_eq!(Style::Window.sequence(true), "38;5;250;48;5;16");
        assert_eq!(Style::Window.sequence(false), "37;40");
        // The bars are a dark grey, not the mid grey of bright black.
        assert_eq!(Style::MenuBar.sequence(true), "38;5;231;48;5;236");
    }

    /// Every style has to be a pair any colour terminal can show: one
    /// foreground, one background, nothing else.
    #[test]
    fn every_style_is_a_plain_colour_pair() {
        for style in [
            Style::MenuBar,
            Style::MenuAlert,
            Style::Window,
            Style::WindowDim,
            Style::WindowGood,
            Style::WindowWarn,
            Style::WindowAlert,
            Style::Accent,
            Style::Frame,
            Style::FrameAlert,
            Style::Title,
            Style::Selected,
            Style::TabActive,
            Style::TabKey,
            Style::Field,
            Style::FieldDim,
            Style::Badge,
            Style::Dialog,
            Style::DialogDim,
            Style::DialogHot,
            Style::DialogAlert,
            Style::DialogFrame,
            Style::Button,
            Style::ButtonHot,
            Style::StatusKey,
            Style::StatusLabel,
            Style::StatusGood,
            Style::StatusAlert,
            Style::Scrollbar,
            Style::ScrollThumb,
        ] {
            let codes = style.codes();
            let parts: Vec<&str> = codes.split(';').collect();
            assert_eq!(parts.len(), 2, "{style:?} is {codes}");
            let fg: u16 = parts[0].parse().expect("a number");
            let bg: u16 = parts[1].parse().expect("a number");
            assert!(
                (30..=37).contains(&fg) || (90..=97).contains(&fg),
                "{style:?} foreground {fg}"
            );
            assert!(
                (40..=47).contains(&bg) || (100..=107).contains(&bg),
                "{style:?} background {bg}"
            );
        }
    }
}
