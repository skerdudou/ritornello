//! État partagé entre la moitié `display` (qui reçoit les trames du cœur) et
//! les sessions clientes MPD (qui répondent aux commandes de lecture).
//!
//! **Strict minimum pour la Task 3** : juste de quoi compiler et donner à
//! `AfficheurMpd` un endroit où déposer l'état reçu. La Task 5 fait grossir
//! ce fichier *sur place* — compteurs de version par sous-système, `Notify`
//! pour réveiller les sessions en attente d'un changement, lecture optimiste
//! pendant la lecture — ce n'est pas construit ici, volontairement : ce
//! serait improviser une conception que la Task 5 doit encore poser.

use ritornello_proto::PlayerState;
use tokio::sync::RwLock;

/// Dernier état poussé par le cœur, partagé par toutes les sessions clientes.
#[derive(Default)]
pub struct EtatPartage {
    etat: RwLock<PlayerState>,
}

impl EtatPartage {
    /// Copie de l'état courant. Une copie et non une garde : aucune session
    /// ne doit retenir le verrou au-delà de l'instant de la lecture, même si
    /// elle compose ensuite une réponse longue.
    ///
    /// Sans appelant en Task 3 : c'est chaque session cliente (Task 8) qui
    /// l'invoquera pour répondre à `status`. Gardé public et testé dès
    /// maintenant, comme le demande l'interface de cette tâche.
    #[allow(dead_code)]
    pub async fn lire(&self) -> PlayerState {
        self.etat.read().await.clone()
    }

    /// Remplace l'état par celui reçu du cœur.
    pub async fn appliquer_etat(&self, etat: PlayerState) {
        *self.etat.write().await = etat;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lire_rend_letat_par_defaut_avant_toute_application() {
        let partage = EtatPartage::default();
        assert_eq!(partage.lire().await, PlayerState::default());
    }

    #[tokio::test]
    async fn appliquer_etat_remplace_ce_que_lire_rend_ensuite() {
        let partage = EtatPartage::default();
        let nouvel_etat = PlayerState { volume: 42, source: "radio".into(), ..Default::default() };

        partage.appliquer_etat(nouvel_etat.clone()).await;

        assert_eq!(partage.lire().await, nouvel_etat);
    }
}
