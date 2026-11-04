//! The US states and the District of Columbia, by the full name ipinfo gives and the code EIA takes.

const STATES: [(&str, &str); 51] = [
    ("Alabama", "AL"),
    ("Alaska", "AK"),
    ("Arizona", "AZ"),
    ("Arkansas", "AR"),
    ("California", "CA"),
    ("Colorado", "CO"),
    ("Connecticut", "CT"),
    ("Delaware", "DE"),
    ("District of Columbia", "DC"),
    ("Florida", "FL"),
    ("Georgia", "GA"),
    ("Hawaii", "HI"),
    ("Idaho", "ID"),
    ("Illinois", "IL"),
    ("Indiana", "IN"),
    ("Iowa", "IA"),
    ("Kansas", "KS"),
    ("Kentucky", "KY"),
    ("Louisiana", "LA"),
    ("Maine", "ME"),
    ("Maryland", "MD"),
    ("Massachusetts", "MA"),
    ("Michigan", "MI"),
    ("Minnesota", "MN"),
    ("Mississippi", "MS"),
    ("Missouri", "MO"),
    ("Montana", "MT"),
    ("Nebraska", "NE"),
    ("Nevada", "NV"),
    ("New Hampshire", "NH"),
    ("New Jersey", "NJ"),
    ("New Mexico", "NM"),
    ("New York", "NY"),
    ("North Carolina", "NC"),
    ("North Dakota", "ND"),
    ("Ohio", "OH"),
    ("Oklahoma", "OK"),
    ("Oregon", "OR"),
    ("Pennsylvania", "PA"),
    ("Rhode Island", "RI"),
    ("South Carolina", "SC"),
    ("South Dakota", "SD"),
    ("Tennessee", "TN"),
    ("Texas", "TX"),
    ("Utah", "UT"),
    ("Vermont", "VT"),
    ("Virginia", "VA"),
    ("Washington", "WA"),
    ("West Virginia", "WV"),
    ("Wisconsin", "WI"),
    ("Wyoming", "WY"),
];

/// Other spellings of the district: GeoNames' name for it, which ipinfo is thought to use, and its
/// short form
const DISTRICT_SPELLINGS: [&str; 2] = ["Washington, D.C.", "Washington DC"];

/// The two-letter code of the state or district called `name`
pub(super) fn code(name: &str) -> Option<&'static str> {
    if DISTRICT_SPELLINGS.contains(&name) {
        return Some("DC");
    }
    STATES
        .iter()
        .find_map(|(full, code)| (*full == name).then_some(*code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_and_the_district_has_its_own_name_and_code() {
        let mut names: Vec<_> = STATES.iter().map(|(name, _)| *name).collect();
        let mut codes: Vec<_> = STATES.iter().map(|(_, code)| *code).collect();
        names.sort_unstable();
        names.dedup();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!((names.len(), codes.len()), (51, 51));
    }

    #[test]
    fn a_state_reads_as_its_code_and_a_territory_as_none() {
        assert_eq!(code("New Mexico"), Some("NM"));
        assert_eq!(code("District of Columbia"), Some("DC"));
        assert_eq!(code("Washington, D.C."), Some("DC"));
        assert_eq!(code("Washington"), Some("WA"));
        assert_eq!(code("Puerto Rico"), None);
        assert_eq!(code("texas"), None);
    }
}
