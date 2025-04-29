use std::fmt::{Display, Error, Formatter};

#[allow(dead_code)]
pub enum BbCode<'a> {
    Bold(&'a dyn Display),
    Italic(&'a dyn Display),
    Underline(&'a dyn Display),
    Link(&'a dyn Display, &'a str),
}

impl Display for BbCode<'_> {
    fn fmt(&self, fmt: &mut Formatter) -> Result<(), Error> {
        match self {
            BbCode::Bold(text) => fmt.write_fmt(format_args!("[B]{}[/B]", text))?,
            BbCode::Italic(text) => fmt.write_fmt(format_args!("[I]{}[/I]", text))?,
            BbCode::Underline(text) => fmt.write_fmt(format_args!("[U]{}[/U]", text))?,
            BbCode::Link(text, url) => {
                fmt.write_fmt(format_args!("[URL={}]{}[/URL]", url, text))?
            }
        };

        Ok(())
    }
}

#[allow(dead_code)]
pub fn bold(text: &dyn Display) -> BbCode {
    BbCode::Bold(text)
}

#[allow(dead_code)]
pub fn italic(text: &dyn Display) -> BbCode {
    BbCode::Italic(text)
}

#[allow(dead_code)]
pub fn underline(text: &dyn Display) -> BbCode {
    BbCode::Underline(text)
}

#[allow(dead_code)]
pub fn link<'a>(text: &'a dyn Display, url: &'a str) -> BbCode<'a> {
    BbCode::Link(text, url)
}
