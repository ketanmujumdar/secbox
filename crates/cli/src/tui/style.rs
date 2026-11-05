//! The sixteen colours a text terminal of the period actually had.
//!
//! Each style is a foreground and background pair, written as the escape
//! codes a terminal understands. Keeping them in one place means the layout
//! never writes an escape sequence itself, so a width calculation cannot
//! count one by mistake.

/// A colour pair. The names are roles, not colours, so the palette can change
/// in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// The patterned background behind every window.
    Desktop,
    MenuBar,
    /// The letter that chooses a menu entry.
    MenuHot,
    MenuActive,
    /// Inside a window.
    Window,
    WindowDim,
    WindowGood,
    WindowWarn,
    WindowAlert,
    Frame,
    Title,
    /// The highlighted row of a list.
    Selected,
    /// The strip of shadow a window casts.
    Shadow,
    /// A dialog box, which is grey rather than blue.
    Dialog,
    DialogHot,
    DialogFrame,
    Button,
    ButtonHot,
    /// The bar along the bottom: the number, then the word.
    StatusKey,
    StatusLabel,
    Scrollbar,
    ScrollThumb,
}

impl Style {
    /// The escape sequence that turns this style on.
    pub fn codes(self) -> &'static str {
        match self {
            // Grey pattern on blue, the background of every one of these.
            Style::Desktop => "90;44",
            Style::MenuBar => "30;47",
            Style::MenuHot => "31;47",
            Style::MenuActive => "30;46",
            Style::Window => "37;44",
            Style::WindowDim => "36;44",
            Style::WindowGood => "92;44",
            Style::WindowWarn => "93;44",
            Style::WindowAlert => "97;41",
            Style::Frame => "37;44",
            Style::Title => "93;44",
            Style::Selected => "30;46",
            Style::Shadow => "30;40",
            Style::Dialog => "30;47",
            Style::DialogHot => "31;47",
            Style::DialogFrame => "30;47",
            Style::Button => "97;42",
            Style::ButtonHot => "93;42",
            Style::StatusKey => "97;40",
            Style::StatusLabel => "30;46",
            Style::Scrollbar => "34;44",
            Style::ScrollThumb => "97;44",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Style;

    /// Every style has to be one of the pairs a text terminal of the time
    /// could show: one foreground, one background, nothing else.
    #[test]
    fn every_style_is_a_plain_colour_pair() {
        for style in [
            Style::Desktop,
            Style::MenuBar,
            Style::MenuHot,
            Style::MenuActive,
            Style::Window,
            Style::WindowDim,
            Style::WindowGood,
            Style::WindowWarn,
            Style::WindowAlert,
            Style::Frame,
            Style::Title,
            Style::Selected,
            Style::Shadow,
            Style::Dialog,
            Style::DialogHot,
            Style::DialogFrame,
            Style::Button,
            Style::ButtonHot,
            Style::StatusKey,
            Style::StatusLabel,
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
