//! Reading and writing text through the AT-SPI text interfaces.

use harness_desktop::error::DesktopError;
use zbus::blocking::Connection;

use crate::element::AtspiElement;
use crate::tree::node::call;

const TEXT_INTERFACE: &str = "org.a11y.atspi.Text";
const EDITABLE_TEXT_INTERFACE: &str = "org.a11y.atspi.EditableText";
const VALUE_INTERFACE: &str = "org.a11y.atspi.Value";

/// The whole text of an element.
pub fn read_text(
    connection: &Connection,
    element: &AtspiElement,
) -> Result<Option<String>, DesktopError> {
    match call::<String>(
        connection,
        element,
        TEXT_INTERFACE,
        "GetText",
        &(0i32, -1i32),
    ) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.code == harness_desktop::error::ErrorCode::ActionNotSupported => Ok(None),
        Err(err) => Err(err),
    }
}

/// The value of an element, from whichever interface offers one.
///
/// A text field answers on the text interface; a slider answers on the value
/// interface. Asking both is how one read covers every kind of control.
pub fn read_value(
    connection: &Connection,
    element: &AtspiElement,
) -> Result<Option<String>, DesktopError> {
    if let Ok(value) = call::<f64>(connection, element, VALUE_INTERFACE, "GetCurrentValue", &()) {
        return Ok(Some(format_value(value)));
    }
    read_text(connection, element)
}

/// Replace the whole text of an element.
pub fn set_text_contents(
    connection: &Connection,
    element: &AtspiElement,
    text: &str,
) -> Result<bool, DesktopError> {
    let done: bool = call(
        connection,
        element,
        EDITABLE_TEXT_INTERFACE,
        "SetTextContents",
        &(text,),
    )?;
    Ok(done)
}

/// Set the value of an element that has one.
pub fn set_value(
    connection: &Connection,
    element: &AtspiElement,
    value: f64,
) -> Result<bool, DesktopError> {
    let done: bool = call(
        connection,
        element,
        VALUE_INTERFACE,
        "SetCurrentValue",
        &(value,),
    )?;
    Ok(done)
}

/// A number without a trailing `.0`, so a slider reads as `5` and not `5.0`.
fn format_value(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_number_reads_without_a_fraction() {
        assert_eq!(format_value(5.0), "5");
        assert_eq!(format_value(-3.0), "-3");
    }

    #[test]
    fn a_fraction_reads_as_a_fraction() {
        assert_eq!(format_value(2.5), "2.5");
    }
}
