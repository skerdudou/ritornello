use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub line1: String,
    pub line2: String,
    pub line3: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_json() {
        let v = View { line1: "RADIO  P1".into(), line2: "FIP".into(), line3: "".into() };
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<View>(&json).unwrap(), v);
    }
}
