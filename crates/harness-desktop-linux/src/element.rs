/// One object on the accessibility bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtspiElement {
    /// The bus name the application owns.
    pub bus_name: String,
    /// The object path of the accessible.
    pub path: String,
}

/// The separator between the bus name and the path in an encoded element.
///
/// A pipe cannot appear in a D-Bus name or in an object path, so it cannot be
/// confused with the colon that bus names like `:1.42` carry.
const SEPARATOR: char = '|';

impl AtspiElement {
    pub fn new(bus_name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            bus_name: bus_name.into(),
            path: path.into(),
        }
    }

    /// An id that is stable for the life of the object.
    pub fn id(&self) -> String {
        format!("{}:{}", self.bus_name, self.path)
    }

    /// The element as one token, for the core's node handle.
    pub fn encode(&self) -> String {
        format!("{}{SEPARATOR}{}", self.bus_name, self.path)
    }

    /// Read an element back from the token the core carried.
    pub fn decode(token: &str) -> Option<Self> {
        let (bus_name, path) = token.split_once(SEPARATOR)?;
        if bus_name.is_empty() || path.is_empty() {
            return None;
        }
        Some(Self::new(bus_name, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_element_id_names_its_bus_and_path() {
        let element = AtspiElement::new(":1.42", "/org/a11y/atspi/accessible/17");
        assert_eq!(element.id(), ":1.42:/org/a11y/atspi/accessible/17");
    }

    #[test]
    fn an_element_survives_a_round_trip_through_its_token() {
        let element = AtspiElement::new(":1.42", "/org/a11y/atspi/accessible/17");
        let decoded = AtspiElement::decode(&element.encode()).expect("a decoded element");
        assert_eq!(decoded, element);
    }

    #[test]
    fn a_token_without_the_separator_is_not_an_element() {
        assert!(AtspiElement::decode(":1.42:/org/a11y/atspi/accessible/17").is_none());
    }

    #[test]
    fn a_token_with_an_empty_half_is_not_an_element() {
        assert!(AtspiElement::decode("|/org/a11y/atspi/accessible/17").is_none());
        assert!(AtspiElement::decode(":1.42|").is_none());
    }
}
