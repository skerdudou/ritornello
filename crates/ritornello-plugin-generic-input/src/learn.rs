use serde::Serialize;

/// État d'apprentissage tel que l'IHM le lit dans `GetData` : `captured` reste
/// `null` tant qu'aucune touche n'a été pressée.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Learning {
    pub device: String,
    pub captured: Option<u16>,
}

/// Machine à états de l'apprentissage. Pure : aucune I/O, entièrement
/// testable sans matériel. L'apprentissage est **exclusif** — une nouvelle
/// demande remplace la précédente.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LearnState {
    current: Option<Learning>,
}

impl LearnState {
    /// Entre (ou ré-entre) en apprentissage pour ce périphérique.
    pub fn learn(&mut self, device: &str) {
        self.current = Some(Learning { device: device.to_string(), captured: None });
    }

    /// Sort de l'apprentissage sans rien retenir.
    pub fn cancel(&mut self) {
        self.current = None;
    }

    /// Abandonne l'apprentissage s'il visait ce périphérique (utilisé quand le
    /// périphérique disparaît).
    pub fn cancel_if(&mut self, device: &str) {
        if self.current.as_ref().is_some_and(|l| l.device == device) {
            self.current = None;
        }
    }

    /// Périphérique dont les événements doivent être **supprimés**, c'est-à-dire
    /// celui en apprentissage tant qu'aucun code n'a été capturé. Une fois le
    /// code capturé l'apprentissage est terminé : le périphérique réémet.
    pub fn device(&self) -> Option<&str> {
        match &self.current {
            Some(l) if l.captured.is_none() => Some(l.device.as_str()),
            _ => None,
        }
    }

    /// Retient le premier code pressé sur le périphérique visé. Renvoie `true`
    /// si l'événement a été consommé par l'apprentissage.
    pub fn capture(&mut self, device: &str, code: u16) -> bool {
        match &mut self.current {
            Some(l) if l.device == device && l.captured.is_none() => {
                l.captured = Some(code);
                tracing::info!("apprentissage: {device} -> code {code}");
                true
            }
            _ => false,
        }
    }

    /// Copie de l'état pour `GetData` (`None` hors apprentissage).
    pub fn snapshot(&self) -> Option<Learning> {
        self.current.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learn_puis_capture_retient_le_premier_code() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        assert_eq!(s.device(), Some("USB Keyboard"));
        assert_eq!(s.snapshot(), Some(Learning { device: "USB Keyboard".into(), captured: None }));
        assert!(s.capture("USB Keyboard", 115));
        assert_eq!(
            s.snapshot(),
            Some(Learning { device: "USB Keyboard".into(), captured: Some(115) })
        );
        // deuxième appui : plus rien à capturer, l'apprentissage est terminé
        assert!(!s.capture("USB Keyboard", 42));
        assert_eq!(s.snapshot().unwrap().captured, Some(115));
        // et le périphérique réémet ses commandes
        assert_eq!(s.device(), None);
    }

    #[test]
    fn capture_ignore_les_autres_peripheriques() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        assert!(!s.capture("eHome", 115));
        assert_eq!(s.snapshot().unwrap().captured, None);
    }

    #[test]
    fn capture_sans_apprentissage_ne_fait_rien() {
        let mut s = LearnState::default();
        assert!(!s.capture("USB Keyboard", 115));
        assert_eq!(s.snapshot(), None);
    }

    #[test]
    fn cancel_efface_letat() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.cancel();
        assert_eq!(s.snapshot(), None);
        assert_eq!(s.device(), None);
    }

    #[test]
    fn un_nouveau_learn_remplace_le_precedent() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.capture("USB Keyboard", 115);
        s.learn("eHome");
        assert_eq!(s.snapshot(), Some(Learning { device: "eHome".into(), captured: None }));
        assert_eq!(s.device(), Some("eHome"));
    }

    #[test]
    fn cancel_if_nabandonne_que_le_peripherique_vise() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.cancel_if("eHome");
        assert_eq!(s.device(), Some("USB Keyboard"));
        s.cancel_if("USB Keyboard");
        assert_eq!(s.snapshot(), None);
    }

    #[test]
    fn snapshot_se_serialise_comme_attendu() {
        let mut s = LearnState::default();
        assert_eq!(serde_json::to_value(s.snapshot()).unwrap(), serde_json::Value::Null);
        s.learn("USB Keyboard");
        assert_eq!(
            serde_json::to_value(s.snapshot()).unwrap(),
            serde_json::json!({ "device": "USB Keyboard", "captured": null })
        );
        s.capture("USB Keyboard", 115);
        assert_eq!(
            serde_json::to_value(s.snapshot()).unwrap(),
            serde_json::json!({ "device": "USB Keyboard", "captured": 115 })
        );
    }
}
