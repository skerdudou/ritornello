import { getCurrentInstance, onUnmounted, ref } from 'vue'
import type { PlayerPayload } from '../types'

/**
 * Duree en `m:ss`, ou `null` si inconnue.
 *
 * Pas d'heures : ce sont des morceaux de musique, et un affichage `0:03:34`
 * serait plus long a lire pour rien. Une duree negative ou absurde est traitee
 * comme inconnue plutot que rendue telle quelle — elle vient d'un tiers.
 */
export function formateDuree(secondes: number | null | undefined): string | null {
  if (typeof secondes !== 'number' || !Number.isFinite(secondes) || secondes <= 0) return null
  const m = Math.floor(secondes / 60)
  const s = Math.floor(secondes % 60)
  return `${m}:${String(s).padStart(2, '0')}`
}

/**
 * Vrai si l'etat n'apprend rien d'affichable.
 *
 * La duree seule ne compte pas : « 3:34 » sans titre ni artiste n'informe
 * personne, et afficherait un bloc vide.
 */
export function riendAfficher(etat: PlayerPayload | null): boolean {
  if (!etat) return true
  return !etat.artist && !etat.title && !etat.album
}

/**
 * Etat du lecteur, recu en flux pousse depuis `/api/player`.
 *
 * `EventSource` plutot qu'un sondage : le coeur pousse deja chaque changement,
 * et le navigateur se reconnecte tout seul apres une coupure — aucune logique
 * de reprise a ecrire ici. L'etat courant arrive des la connexion, donc un
 * onglet ouvert au milieu d'un morceau ne reste pas vide.
 */
export function usePlayer() {
  const etat = ref<PlayerPayload | null>(null)
  let flux: EventSource | null = null

  function ouvre(): void {
    // `EventSource` n'existe pas partout (jsdom sous test, vieux moteurs) :
    // l'absence du morceau en cours ne doit pas casser le reste de la page.
    if (typeof EventSource === 'undefined') {
      console.warn('EventSource indisponible : le morceau en cours ne sera pas affiche')
      return
    }
    ferme()
    flux = new EventSource('/api/player')
    flux.onmessage = (e: MessageEvent) => {
      try {
        etat.value = JSON.parse(e.data as string) as PlayerPayload
      } catch {
        // Trame illisible : on garde l'affichage precedent plutot que de le vider.
      }
    }
    // Aucun traitement d'erreur : `EventSource` reprend de lui-meme, et
    // fermer ici priverait la page de toute reprise apres un redemarrage du
    // coeur (cas le plus courant : `systemctl restart ritornello`).
  }

  function ferme(): void {
    flux?.close()
    flux = null
  }

  // Utilisable hors composant (tests) : `onUnmounted` sans instance courante
  // provoquerait un avertissement de Vue.
  if (getCurrentInstance()) onUnmounted(ferme)

  return { etat, ouvre, ferme }
}
