# Plugin fichiers : navigateur, retour arrière et manuel forcé — plan d'implémentation

> **Pour les agents exécutants :** SOUS-COMPÉTENCE REQUISE : utiliser
> superpowers:subagent-driven-development pour dérouler ce plan tâche par
> tâche. Les étapes sont des cases à cocher (`- [ ]`).

**But :** quatre corrections d'usage sur l'IHM d'admin du plugin fichiers — la
liste en tête, une source refusée qui ne s'inscrit plus, l'assistant réseau
réduit au manuel quand `smbclient` manque, et le volet Parcourir transformé en
navigateur de fichiers dont la recherche ne s'écrase plus sur le plafond de la
liste de lecture.

**Architecture :** rien de nouveau côté protocole sauf un champ `path` sur
l'opération `search` et un champ `query` en retour, qui rendent la recherche
relative au dossier ouvert et permettent à la page de n'accepter que la réponse
qu'elle vient de demander. Le reste est local : une permutation de gabarit, un
retour arrière dans la branche `AddSource`, une marche filtrée dans `scan.rs`,
et un volet Vue réécrit sans arbre.

**Pile :** Rust (tokio, serde_json) côté plugin ; Vue 3 `<script setup>` +
Vitest côté IHM ; Playwright pour le parcours de bout en bout.

**Spec :** ce document — le design a été validé en conversation, il est repris
mot pour mot dans « Design retenu » ci-dessous.

## Contraintes globales

- **Langue.** Code, commentaires, noms de symboles et messages de commit en
  **français**, sans accent dans les messages de commit (convention du dépôt).
  Les catalogues i18n : `crates/ritornello-plugin-files/src/locales/en.toml` en
  anglais, `deploy/locales/files/fr.toml` en français.
- **Parité i18n.** Toute clé ajoutée doit l'être **dans les deux** catalogues :
  le test `parite_des_cles_entre_len_embarque_et_le_pack_fr`
  (`crates/ritornello-plugin-files/src/lib.rs:46`) échoue sinon. Toute clé
  employée par l'IHM doit exister dans `en.toml`, sans quoi
  `src/i18nKeysUsed.test.ts` échoue. Toute clé employée par un **test** d'IHM
  doit être ajoutée à `CATALOGUE` dans `src/harnais.ts`, sinon `createT` rend
  la clé brute et l'assertion sur le libellé échoue.
- **Commentaires.** Le dépôt documente le **pourquoi**, en particulier le
  symptôme mesuré qu'un correctif encode. Suivre ce ton : pas de paraphrase du
  code.
- **TDD strict.** Un test qui échoue d'abord, la raison de son échec vérifiée,
  puis l'implémentation minimale, puis le test au vert, puis le commit.
- **Commandes.** `cargo` n'existe **que dans WSL** :
  `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-plugin-files"`.
  Les tests d'IHM se lancent avec `npx vitest run` **depuis**
  `crates/ritornello-plugin-files/ui` (jamais depuis la racine du worktree :
  vitest ratisse alors tout l'arbre sans plugin Vue). La jonction
  `node_modules/@ritornello/ui` → `web/kit` du worktree est **déjà créée**.
- **Baseline.** 60 tests Rust et 127 tests d'IHM passent au départ. Aucun ne
  doit finir rouge, sauf ceux que ce plan demande explicitement de réécrire.
- **Ne pas toucher** au `dist/` du plugin avant la tâche 7. Il n'est **pas**
  suivi par git (vérifié : `git ls-files crates/ritornello-plugin-files/ui/dist`
  ne rend rien, comme pour `web/app/dist` et `web/kit/dist`) mais `admin.rs`
  l'embarque par `include_str!` : il doit donc exister sur le disque pour que
  `cargo build` aboutisse, et il se reconstruit une fois à la fin — sans être
  commité.

**Ordre d'exécution** : tâches 1 à 6, puis **8 et 9**, puis **7**. Les tâches 8
et 9 ont été ajoutées après coup (correction des messages d'erreur, feu vert
donné en cours de chantier) ; la tâche 7, qui reconstruit les paquets et lance
le parcours de bout en bout, reste le sceau final et doit donc passer en
dernier.

## Design retenu

1. **Ordre des volets** : la liste en cours passe en tête, puis Sources, puis
   Parcourir.
2. **Une source refusée ne s'inscrit pas** : si le partage n'est pas monté à
   l'issue de la déclaration, la déclaration est défaite (table, fichier
   d'identifiants) et le refus remonte à la popin, qui reste ouverte avec la
   saisie. Portée limitée à `add_source` : une source déjà acceptée reste
   jusqu'à suppression manuelle, et un partage tiers en panne n'annule pas
   l'ajout d'un partage sain.
3. **Assistant réseau indisponible** : sans `smbclient`, l'IHM est d'emblée en
   mode manuel, sans bascule ni bouton de connexion. Le champ `domain` reste où
   il est, avec un libellé qui dit ce qu'il est (domaine Windows, optionnel).
4. **Volet Parcourir** : un navigateur de fichiers — un seul niveau à l'écran,
   bouton « remonter », chemin courant, hauteur bornée et défilement — au lieu
   d'un arbre qu'on déplie. La recherche porte sur le **dossier ouvert** et
   s'affiche sous lui.
5. **Recherche** : `scan::search` ne réutilise plus `MAX_TRACKS` (le plafond de
   la *liste de lecture*) comme plafond de *marche*. Symptôme mesuré : chercher
   à la racine d'un NAS de plus de 2000 pistes renvoyait « this folder holds
   more than 2000 tracks: narrow it down, or add its subfolders one by one », le
   message de l'**ajout**, pour une recherche qui n'ajoute rien.

Hors périmètre, décidé explicitement : la suppression des `stat` par fichier
dans la marche, et un index de chemins maintenu par le plugin.

---

### Tâche 1 : la liste de lecture en tête

**Fichiers :**
- Modifier : `crates/ritornello-plugin-files/ui/src/FilesAdmin.vue` (bloc
  `<template v-if="donnees">`, en fin de fichier)
- Test : `crates/ritornello-plugin-files/ui/src/FilesAdmin.test.ts`

**Interfaces :**
- Consomme : rien.
- Produit : rien. Les trois volets gardent leurs props et leurs attributs
  `data-volet-liste`, `data-volet-sources`, `data-volet-parcourir`.

- [ ] **Étape 1 : écrire le test qui échoue**

À ajouter dans `FilesAdmin.test.ts`, dans le `describe` existant (reprendre le
`monter` déjà importé par ce fichier) :

```ts
it('présente la liste en cours avant les deux autres volets', async () => {
  // L'ordre est celui de l'usage : on regarde ce qui joue, puis on complète.
  // Déclarer une source est rare, parcourir vient après avoir vu la liste.
  const { w } = await monter({ roots: [{ name: 'nas', kind: 'local', path: '/m' }] })
  const ordre = w
    .findAll('[data-volet-liste],[data-volet-sources],[data-volet-parcourir]')
    .map((s) => Object.keys(s.attributes()).find((a) => a.startsWith('data-volet')))
  expect(ordre).toEqual(['data-volet-liste', 'data-volet-sources', 'data-volet-parcourir'])
})
```

- [ ] **Étape 2 : le lancer et vérifier qu'il échoue**

Depuis `crates/ritornello-plugin-files/ui` :
`npx vitest run src/FilesAdmin.test.ts`
Attendu : ÉCHEC, l'ordre observé est
`['data-volet-sources', 'data-volet-parcourir', 'data-volet-liste']`.

- [ ] **Étape 3 : permuter le gabarit**

Dans `FilesAdmin.vue`, déplacer le bloc `<VoletListe .../>` **avant**
`<VoletSources .../>`, en gardant `<VoletParcourir .../>` en dernier. Les props
ne changent pas. Résultat attendu :

```html
    <template v-if="donnees">
      <VoletListe
        :donnees="donnees"
        :t="t"
        :envoyer="envoyer"
        :fige="chargementEchoue || enCours"
        :est-source-active="estSourceActive"
      />
      <VoletSources
        :donnees="donnees"
        :t="t"
        :envoyer="envoyer"
        :fige="chargementEchoue || enCours"
        :message="message"
      />
      <VoletParcourir
        :donnees="donnees"
        :t="t"
        :envoyer="envoyer"
        :fige="chargementEchoue || enCours"
      />
    </template>
```

- [ ] **Étape 4 : tests au vert**

`npx vitest run` depuis `crates/ritornello-plugin-files/ui`
Attendu : 10 fichiers, 128 tests, tous verts.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-plugin-files/ui/src/FilesAdmin.vue crates/ritornello-plugin-files/ui/src/FilesAdmin.test.ts
git commit -m "feat(files): la liste en cours passe en tete de la page"
```

---

### Tâche 2 : sans smbclient, la saisie manuelle seule

**Fichiers :**
- Modifier : `crates/ritornello-plugin-files/ui/src/DialoguePartage.vue`
- Modifier : `crates/ritornello-plugin-files/src/locales/en.toml:46`
- Modifier : `deploy/locales/files/fr.toml:45`
- Modifier : `crates/ritornello-plugin-files/ui/src/harnais.ts` (clé `ph_domain`
  de `CATALOGUE`)
- Test : `crates/ritornello-plugin-files/ui/src/DialoguePartage.test.ts`

**Interfaces :**
- Consomme : `donnees.canBrowseSmb` (déjà exposé par `donnees.ts:402`).
- Produit : rien. Aucun changement de protocole ; la charge d'`add_source`
  émise en mode manuel reste identique, `domain` compris.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `DialoguePartage.test.ts`, **remplacer** les deux tests existants
« sans smbclient l'assistant est grisé et la raison est nommée » et « le repli
manuel reste offert sans smbclient » par ceux-ci :

```ts
it('sans smbclient la popin est d’emblée en saisie manuelle', async () => {
  // Plus de bouton grisé à comprendre : il n'y a rien à parcourir, donc rien
  // à basculer. La raison reste nommée, elle explique pourquoi les champs
  // remplacent l'assistant.
  const { envoyer } = await monter({ can_browse_smb: false })
  expect((dansPopin('[data-smb-unavailable]')?.textContent ?? '').length).toBeGreaterThan(0)
  expect(dansPopin('[data-manual-share]')).not.toBeNull()
  expect(dansPopin('[data-connect]')).toBeNull()
  expect(dansPopin('[data-manuel]')).toBeNull()
  expect(envoyer).not.toHaveBeenCalled()
})

it('avec smbclient la bascule manuelle reste offerte, et l’assistant est le défaut', async () => {
  await monter({ can_browse_smb: true })
  expect(dansPopin('[data-manual-share]')).toBeNull()
  expect(dansPopin('[data-manuel]')).not.toBeNull()
  await cliquerPopin('[data-manuel]')
  expect(dansPopin('[data-manual-share]')).not.toBeNull()
})

it('le champ domaine dit qu’il est optionnel', async () => {
  // Signalé à l'usage : « domaine » seul ne dit pas à quoi il sert, et se lit
  // comme un champ à remplir. Il ne sert qu'à un compte de domaine Windows.
  await monter({ can_browse_smb: true })
  expect(dansPopin('[data-domain]')?.getAttribute('placeholder')).toContain('optionnel')
})
```

Puis, dans les trois tests qui suivent (« la saisie manuelle déclare la source
directement », « un sous-chemin manuel laissé vide part à null… » et tout autre
test montant avec `can_browse_smb: false`), **supprimer** la ligne
`await cliquerPopin('[data-manuel]')` : la popin y est déjà. Vérifier le
fichier entier à la recherche de `[data-manuel]` avant de conclure.

- [ ] **Étape 2 : les lancer et vérifier qu'ils échouent**

`npx vitest run src/DialoguePartage.test.ts`
Attendu : ÉCHEC — `[data-manual-share]` est `null` à l'ouverture, `[data-manuel]`
existe encore, et le placeholder du domaine ne contient pas « optionnel ».

- [ ] **Étape 3 : implémenter**

Dans `DialoguePartage.vue`, remplacer `const manuel = ref(false)` par :

```ts
/**
 * Le mode manuel est **imposé** quand l'assistant ne peut pas fonctionner.
 *
 * Sans `smbclient` il n'y a rien à parcourir : offrir une bascule vers un
 * assistant inerte, et un bouton « Se connecter » grisé, donnait deux
 * commandes à comprendre pour un choix qui n'existe pas. La raison reste
 * affichée (`smb_unavailable`) : c'est elle qui explique pourquoi les champs
 * remplacent l'assistant.
 */
const manuelForce = computed(() => !props.donnees.canBrowseSmb)
const manuelChoisi = ref(false)
const manuel = computed(() => manuelForce.value || manuelChoisi.value)
```

Dans le `watch` de remise à zéro, remplacer `manuel.value = false` par
`manuelChoisi.value = false`.

Dans le gabarit, encadrer le bouton de bascule d'un `v-if` :

```html
        <Button v-if="!manuelForce" variant="ghost" data-manuel @click="manuelChoisi = !manuelChoisi">
          {{ manuel ? t('btn_assistant') : t('btn_manual') }}
        </Button>
```

Le bouton `data-connect` porte déjà `v-if="!manuel"` : il disparaît de lui-même.
Retirer de sa liste de `:disabled` la condition `!donnees.canBrowseSmb`, devenue
morte — il n'est plus rendu dans ce cas :

```html
        <Button
          v-if="!manuel"
          variant="secondary"
          data-connect
          :disabled="fige || ex.busy"
          @click="connecter"
        >
```

Libellé du champ, dans les deux catalogues et dans le harnais :

- `crates/ritornello-plugin-files/src/locales/en.toml` :
  `ph_domain = "Windows domain (optional)"`
- `deploy/locales/files/fr.toml` :
  `ph_domain = "domaine Windows (optionnel)"`
- `crates/ritornello-plugin-files/ui/src/harnais.ts`, dans `CATALOGUE` :
  `ph_domain: 'domaine Windows (optionnel)',`

- [ ] **Étape 4 : tests au vert**

`npx vitest run` depuis `crates/ritornello-plugin-files/ui` — tout vert.
Puis la parité des catalogues :
`wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-plugin-files parite"`
Attendu : 1 test, vert.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-plugin-files/ui/src/DialoguePartage.vue crates/ritornello-plugin-files/ui/src/DialoguePartage.test.ts crates/ritornello-plugin-files/ui/src/harnais.ts crates/ritornello-plugin-files/src/locales/en.toml deploy/locales/files/fr.toml
git commit -m "feat(files): sans smbclient, la declaration de partage est manuelle seule"
```

---

### Tâche 3 : un partage qui ne se monte pas n'est pas déclaré

**Fichiers :**
- Modifier : `crates/ritornello-plugin-files/src/mount.rs:83-95` (fonction `state`)
- Modifier : `crates/ritornello-plugin-files/src/admin.rs` (branche
  `Op::AddSource`, autour de la ligne 700 : `self.reconcilier(&table, false).await;`)
- Modifier : `crates/ritornello-plugin-files/src/locales/en.toml`
- Modifier : `deploy/locales/files/fr.toml`
- Test : `crates/ritornello-plugin-files/src/admin.rs`, `mod tests`

**Interfaces :**
- Consomme : `mount::state(&Root) -> MountState`,
  `volumes::lire_proc_mounts() -> String`, `Roots::by_name`,
  `Root::credentials_path`, `FilesAdmin::ecrire_table`,
  `FilesAdmin::reconcilier`, `FilesAdmin::mot`, et l'utilitaire de test
  `ajout_partage(mot_de_passe)` qui existe déjà dans `mod tests`.
- Produit : deux clés i18n, `share_not_declared` et `mount_silent_failure`,
  employées seulement ici.

- [ ] **Étape 1 : écrire les tests qui échouent**

Ajouter dans `mod tests` d'`admin.rs`. Le verrou est **indispensable** : les
tests d'un même binaire tournent en parallèle dans le même processus, et
`std::env::set_var` est global — sans lui, le faux `/proc/mounts` de ce test
fuiterait dans `get_data_annonce_les_volumes_et_la_capacite_smb` et
réciproquement.

```rust
    /// Sérialise les tests qui détournent `/proc/mounts`.
    ///
    /// `std::env::set_var` est global au processus, et les tests d'un binaire
    /// tournent en parallèle dedans : sans ce verrou, le faux fichier d'un test
    /// est lu par un autre, avec un échec qui ne se reproduit pas seul.
    static VERROU_PROC_MOUNTS: Mutex<()> = Mutex::new(());

    /// Écrit un faux `/proc/mounts` et le fait lire au code sous test.
    ///
    /// Rend le garde du verrou : l'appelant doit le garder vivant jusqu'à la
    /// fin du test (`let _garde = ...`, jamais `let _ = ...`, qui le
    /// relâcherait aussitôt).
    fn detourner_proc_mounts(
        racine: &std::path::Path,
        contenu: &str,
    ) -> std::sync::MutexGuard<'static, ()> {
        let garde = VERROU_PROC_MOUNTS.lock().unwrap_or_else(|e| e.into_inner());
        let faux = racine.join("mounts");
        std::fs::write(&faux, contenu).unwrap();
        std::env::set_var("RITORNELLO_FILES_PROC_MOUNTS", &faux);
        garde
    }

    #[tokio::test]
    async fn un_partage_qui_ne_se_monte_pas_n_est_pas_declare() {
        // Signalé à l'usage : la source apparaissait dans la liste alors que le
        // montage avait échoué, et il fallait la retirer à la main avant de
        // pouvoir réessayer. La déclaration se défait donc entièrement —
        // table et fichier d'identifiants — et le refus remonte à la popin,
        // qui garde la saisie.
        let (mut admin, racine) = admin_de_test();
        let _garde = detourner_proc_mounts(&racine, "proc /proc proc rw 0 0\n");
        let err = admin.set_data(ajout_partage("p")).await.unwrap_err();
        assert!(err.contains(' '), "cle brute renvoyee a l'ecran : {err}");
        assert!(
            admin.roots.read().await.root.is_empty(),
            "la source est restee declaree malgre l'echec du montage"
        );
        assert!(
            !admin.creds_dir.join("musique.cred").exists(),
            "un mot de passe a survecu a une source qui n'existe pas"
        );
        std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS");
    }

    #[tokio::test]
    async fn un_partage_effectivement_monte_reste_declare() {
        // L'autre moitié, et la raison du critère : `systemctl` est global, il
        // peut échouer pour un partage tiers en panne. Ce qui décide est l'état
        // observé de CETTE source, pas le code de retour de la réconciliation.
        // Sans cela, un NAS endormi ailleurs annulerait l'ajout d'un partage
        // sain.
        let (mut admin, racine) = admin_de_test();
        let _garde = detourner_proc_mounts(
            &racine,
            "//192.168.1.20/musique /mnt/ritornello/musique cifs ro,relatime 0 0\n",
        );
        admin.set_data(ajout_partage("p")).await.unwrap();
        assert_eq!(admin.roots.read().await.root.len(), 1);
        std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS");
    }
```

Ajouter aussi le verrou au test existant
`get_data_annonce_les_volumes_et_la_capacite_smb` (vers la ligne 1213) :
remplacer ses lignes d'écriture du faux fichier et de `set_var` par
`let _garde = detourner_proc_mounts(&racine, "/dev/sda1 /media/usb vfat rw 0 0\nproc /proc proc rw 0 0\n");`,
en gardant son `remove_var` final et le reste de ses assertions.

- [ ] **Étape 2 : les lancer et vérifier qu'ils échouent**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-plugin-files declare"
```
Attendu : `un_partage_qui_ne_se_monte_pas_n_est_pas_declare` ÉCHOUE — le
`set_data` rend `Ok`, donc `unwrap_err` panique.
`un_partage_effectivement_monte_reste_declare` passe déjà : il encode le
comportement à **préserver**.

- [ ] **Étape 3 : rendre `mount::state` observable en test**

Dans `mount.rs`, `state` lit `/proc/mounts` par un chemin en dur, que rien ne
peut détourner. Le faire passer par le lecteur qui honore déjà la variable
d'environnement — une seule façon de lire cette table dans tout le crate :

```rust
pub fn state(root: &Root) -> MountState {
    if root.kind == RootKind::Local {
        return MountState::Mounted;
    }
    // Par `volumes::lire_proc_mounts` et non par un `read_to_string` en dur :
    // c'est le seul lecteur de cette table, il honore
    // `RITORNELLO_FILES_PROC_MOUNTS`, et c'est ce qui rend le retour arrière
    // d'une déclaration ratée vérifiable sans monter quoi que ce soit. Une
    // table illisible rend la chaîne vide, donc `NotMounted` : ne pas savoir,
    // c'est ne pas pouvoir promettre que le partage est là.
    if est_monte_dans(&crate::volumes::lire_proc_mounts(), &point_de_montage(root)) {
        MountState::Mounted
    } else {
        MountState::NotMounted
    }
}
```

Vérifier ensuite par `grep -n PROC_MOUNTS crates/ritornello-plugin-files/src/mount.rs`
si la constante `PROC_MOUNTS` de ce fichier a encore un usage ; si elle n'en a
plus, la supprimer (sinon `cargo` avertira d'une constante morte).

- [ ] **Étape 4 : défaire la déclaration quand le partage n'est pas monté**

Dans `admin.rs`, branche `Op::AddSource`, remplacer le bloc de fin :

```rust
                self.ecrire_table(&table)?;
                // Le montage suit la déclaration : plus de bouton à trouver.
                // Un échec ne défait PAS la déclaration — l'utilisateur perdrait
                // sa saisie à cause d'un NAS endormi — il est rapporté à part.
                self.reconcilier(&table, false).await;
                *self.roots.write().await = table;
                Ok(())
```

par :

```rust
                self.ecrire_table(&table)?;
                // Le montage suit la déclaration : plus de bouton à trouver.
                self.reconcilier(&table, false).await;
                // Et s'il n'a pas abouti, la déclaration se défait.
                //
                // Le critère est l'état **observé de cette source**, pas le code
                // de retour de la réconciliation : `systemctl start` porte sur
                // l'unité entière, il peut échouer à cause d'un partage tiers
                // endormi, et annuler alors l'ajout d'un partage sain serait
                // faux. Signalé à l'usage : une source restait inscrite après un
                // montage refusé, et il fallait la retirer à la main avant de
                // pouvoir réessayer.
                //
                // La portée s'arrête à la déclaration. Une source déjà acceptée
                // reste jusqu'à suppression manuelle : un partage momentanément
                // injoignable ne doit pas disparaître de la table.
                if kind == RootKind::Smb
                    && mount::state(table.by_name(&name).expect("tout juste inseree"))
                        != mount::MountState::Mounted
                {
                    let detail = self
                        .mount_error
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| self.mot("mount_silent_failure"));
                    let i = table
                        .root
                        .iter()
                        .position(|r| r.name == name)
                        .expect("la source vient d'etre inseree");
                    let partie = table.root.remove(i);
                    self.ecrire_table(&table)?;
                    // Le fichier d'identifiants part avec elle : le laisser
                    // ferait survivre un mot de passe à une source qui n'a
                    // jamais existé.
                    let _ = std::fs::remove_file(partie.credentials_path(&self.creds_dir));
                    // `aussi: false` à dessein : il n'y a rien à démonter — le
                    // partage n'est justement pas monté — et la garde de
                    // `reconcilier` remet `mount_error` à vide s'il ne reste
                    // aucun partage. Sans cela la page annoncerait un échec de
                    // montage pour une source qui n'est plus déclarée.
                    self.reconcilier(&table, false).await;
                    *self.roots.write().await = table;
                    return Err(format!("{} {detail}", self.mot("share_not_declared")));
                }
                *self.roots.write().await = table;
                Ok(())
```

Les deux clés, à ajouter dans les **deux** catalogues.

`crates/ritornello-plugin-files/src/locales/en.toml` :

```toml
share_not_declared = "the share was not mounted, so it has not been declared."
mount_silent_failure = "systemd reported success but the mount point is missing: check the share name, the credentials, and that cifs-utils is installed."
```

`deploy/locales/files/fr.toml` :

```toml
share_not_declared = "le partage n'a pas ete monte, il n'a donc pas ete declare."
mount_silent_failure = "systemd n'a signale aucune erreur mais le point de montage est absent : verifiez le nom du partage, les identifiants, et que cifs-utils est installe."
```

Placer chaque clé dans la même section que ses voisines de sujet (près de
`mount_error_title`), en suivant l'ordre déjà en place dans chaque fichier — les
relire avant d'insérer.

- [ ] **Étape 5 : tests au vert**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-plugin-files"
```
Attendu : 62 tests, tous verts (60 de départ + 2). Si
`un_partage_effectivement_monte_reste_declare` échoue, vérifier la valeur de
`crate::roots::MOUNT_ROOT` (`grep -n "MOUNT_ROOT" crates/ritornello-plugin-files/src/roots.rs`)
et corriger le faux `/proc/mounts` du test en conséquence — jamais le code de
production.

- [ ] **Étape 6 : commit**

```bash
git add crates/ritornello-plugin-files/src/admin.rs crates/ritornello-plugin-files/src/mount.rs crates/ritornello-plugin-files/src/locales/en.toml deploy/locales/files/fr.toml
git commit -m "fix(files): une source dont le montage echoue n est plus declaree"
```

---

### Tâche 4 : la recherche ne s'écrase plus sur le plafond de la liste

**Fichiers :**
- Modifier : `crates/ritornello-plugin-files/src/scan.rs` (constante près de
  `MAX_TRACKS` ligne 10, fonction `search` lignes 121-143, nouvelle marche
  filtrante à côté de `marche`)
- Modifier : `crates/ritornello-plugin-files/src/admin.rs` (l'unique appel à
  `scan::search`, vers la ligne 798)
- Test : `crates/ritornello-plugin-files/src/scan.rs`, `mod tests`

**Interfaces :**
- Consomme : `scan::is_audio`, `scan::ScanError`, et l'utilitaire de test
  `fichier(dir, chemin_relatif)` qui existe déjà dans `mod tests`.
- Produit :
  - `pub const MAX_VISITES: usize = 50_000;`
  - `pub fn search(dir: &Path, motif: &str, cap: usize, plafond_visites: usize) -> Result<(Vec<PathBuf>, bool), ScanError>`
    — le booléen rendu vaut « tronqué ». **Ne rend plus jamais**
    `ScanError::TooMany`.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `mod tests` de `scan.rs` :

```rust
    #[test]
    fn une_recherche_au_dela_du_plafond_tronque_au_lieu_de_refuser() {
        // Symptôme mesuré sur un vrai NAS : chercher à la racine renvoyait « this
        // folder holds more than 2000 tracks: narrow it down, or add its
        // subfolders one by one » — le message de l'AJOUT — pour une recherche
        // qui n'ajoute rien à la liste. La cause : `search` réutilisait
        // `MAX_TRACKS`, le plafond de la liste de lecture, comme plafond de
        // marche. Une recherche trop large se tronque et le dit ; elle ne se
        // refuse pas.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fichier(dir.path(), &format!("{i}.mp3"));
        }
        let (trouves, tronque) = search(dir.path(), "mp3", 200, 3).expect("aucun refus attendu");
        assert!(tronque, "un plafond atteint doit se dire");
        assert!(!trouves.is_empty(), "des resultats partiels valent mieux que rien");
    }

    #[test]
    fn le_plafond_de_visite_de_la_recherche_depasse_celui_de_la_liste() {
        // Les deux plafonds ne mesurent pas la même chose : `MAX_TRACKS` borne ce
        // qu'on peut AJOUTER, `MAX_VISITES` ce qu'on peut PARCOURIR en cherchant.
        // Les confondre est exactement le défaut corrigé ici.
        assert!(MAX_VISITES > MAX_TRACKS);
    }

    #[test]
    fn une_recherche_rend_les_correspondances_et_seulement_elles() {
        let dir = tempfile::tempdir().unwrap();
        fichier(dir.path(), "A/miles.flac");
        fichier(dir.path(), "A/autre.mp3");
        fichier(dir.path(), "B/sous/MILES live.mp3");
        let (trouves, tronque) = search(dir.path(), "miles", 200, MAX_VISITES).unwrap();
        let mut relatifs: Vec<String> = trouves
            .iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        relatifs.sort();
        // Insensible à la casse, et sur le nom de fichier seul.
        assert_eq!(relatifs, vec!["A/miles.flac", "B/sous/MILES live.mp3"]);
        assert!(!tronque, "trois fichiers ne remplissent aucun plafond");
    }
```

- [ ] **Étape 2 : les lancer et vérifier qu'ils échouent**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-plugin-files recherche"
```
Attendu : ÉCHEC de compilation — `search` prend trois arguments et `MAX_VISITES`
n'existe pas. C'est l'échec recherché ; ne pas le contourner en adaptant les
tests.

- [ ] **Étape 3 : implémenter**

Dans `scan.rs`, ajouter la constante près de `MAX_TRACKS` :

```rust
/// Plafond de **visite** d'une recherche, distinct de `MAX_TRACKS`.
///
/// `MAX_TRACKS` borne ce que la liste de lecture peut contenir ; le confondre
/// avec le coût d'un parcours faisait refuser toute recherche lancée dans un
/// dossier de plus de 2000 pistes, avec le message de l'ajout — mesuré à la
/// racine d'un NAS. Une recherche ne remplit rien : elle n'a besoin que d'une
/// borne qui l'empêche de tourner sans fin, et le dépassement se rapporte
/// comme « tronqué ».
pub const MAX_VISITES: usize = 50_000;
```

Remplacer `search` en entier :

```rust
/// Cherche récursivement les fichiers audio dont le nom contient `motif`
/// (comparaison insensible à la casse).
///
/// Deux bornes, et deux raisons distinctes : `cap` limite ce qu'on **rapporte**
/// à la page, `plafond_visites` ce qu'on accepte de **parcourir**. L'une comme
/// l'autre rend `tronque`, jamais un refus : une liste partielle annoncée comme
/// telle est utile, un refus ne l'est pas.
///
/// Le filtre s'applique **pendant** la marche : collecter d'abord tout le
/// dossier pour ne garder ensuite qu'une poignée de noms était ce qui faisait
/// buter la recherche sur le plafond de la liste de lecture.
pub fn search(
    dir: &Path,
    motif: &str,
    cap: usize,
    plafond_visites: usize,
) -> Result<(Vec<PathBuf>, bool), ScanError> {
    let motif = motif.to_lowercase();
    if motif.is_empty() {
        return Ok((Vec::new(), false));
    }
    let mut out = Vec::new();
    let mut visites = 0usize;
    let mut vus = HashSet::new();
    // `cap + 1` : on en cherche un de plus que ce qu'on rend, pour distinguer
    // « exactement cap résultats » de « il y en avait davantage ». Sans cela une
    // liste complète de cap éléments serait annoncée comme tronquée.
    let arrete = marche_cherchant(
        dir,
        &motif,
        cap + 1,
        plafond_visites,
        &mut out,
        &mut visites,
        &mut vus,
    )?;
    out.truncate(cap);
    Ok((out, arrete))
}

/// Marche filtrante. Rend `true` si elle s'est arrêtée sur une borne.
fn marche_cherchant(
    dir: &Path,
    motif: &str,
    cap: usize,
    plafond_visites: usize,
    out: &mut Vec<PathBuf>,
    visites: &mut usize,
    vus: &mut HashSet<PathBuf>,
) -> Result<bool, ScanError> {
    let canon =
        dir.canonicalize().map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    // Même garde que `marche` : un lien pointant vers un ancêtre ferait tourner
    // la marche en produisant des chemins de plus en plus longs.
    if !vus.insert(canon) {
        return Ok(false);
    }
    let lecture =
        std::fs::read_dir(dir).map_err(|_| ScanError::Io { path: dir.display().to_string() })?;
    let mut sous_dossiers = Vec::new();
    for entree in lecture {
        let Ok(entree) = entree else { continue };
        let chemin = entree.path();
        // `metadata` et non `symlink_metadata`, comme dans `marche` : un lien
        // vers un dossier réel doit être suivi.
        let Ok(meta) = std::fs::metadata(&chemin) else { continue };
        if meta.is_dir() {
            sous_dossiers.push(chemin);
            continue;
        }
        if !(meta.is_file() && is_audio(&chemin)) {
            continue;
        }
        *visites += 1;
        if *visites > plafond_visites {
            return Ok(true);
        }
        let correspond = chemin
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_lowercase().contains(motif));
        if correspond {
            out.push(chemin);
            if out.len() >= cap {
                return Ok(true);
            }
        }
    }
    sous_dossiers.sort();
    for d in sous_dossiers {
        if marche_cherchant(&d, motif, cap, plafond_visites, out, visites, vus)? {
            return Ok(true);
        }
    }
    Ok(false)
}
```

Dans `admin.rs`, l'unique appel devient :

```rust
                    tokio::task::spawn_blocking(move || {
                        scan::search(&base, &query, 200, scan::MAX_VISITES)
                    })
```

- [ ] **Étape 4 : tests au vert**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-plugin-files"
```
Attendu : 65 tests verts. Le test existant
`le_plafond_est_refuse_et_non_tronque_en_silence` porte sur `walk` (l'ajout) et
doit rester vert **sans modification** : le plafond de l'ajout, lui, refuse
toujours.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-plugin-files/src/scan.rs crates/ritornello-plugin-files/src/admin.rs
git commit -m "fix(files): une recherche large se tronque, elle ne se refuse plus"
```

---

### Tâche 5 : la recherche porte sur le dossier ouvert

**Fichiers :**
- Modifier : `crates/ritornello-plugin-files/src/admin.rs` (`Op::Search` dans
  l'énumération vers la ligne 175 ; les branches `Op::Browse` et `Op::Search`
  vers les lignes 773-821)
- Test : `crates/ritornello-plugin-files/src/admin.rs`, `mod tests`

**Interfaces :**
- Consomme : `FilesAdmin::sous_racine(&self, root, path)`,
  `scan::search(dir, motif, cap, plafond_visites)` (tâche 4).
- Produit, dans la charge `browse` rendue par `get_data` :
  - `Op::Search { root: String, path: String (defaut vide), query: String }`
  - la réponse d'une **recherche** porte `"path"` = le dossier cherché et
    `"query"` = le motif ; celle d'un **parcours** porte `"query": ""`.
    C'est ce couple que l'IHM compare pour n'accepter que sa propre demande.
  - les chemins de `"results"` restent **relatifs à la racine**, jamais au
    dossier cherché : c'est ce que `add_file` attend.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `mod tests` d'`admin.rs`. Une racine **locale** suffit : elle ne demande
aucun montage.

```rust
    /// Déclare une racine locale peuplée, et rend son chemin.
    async fn racine_locale_peuplee(admin: &mut FilesAdmin) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        std::mem::forget(dir);
        std::fs::create_dir_all(base.join("A")).unwrap();
        std::fs::create_dir_all(base.join("B")).unwrap();
        std::fs::write(base.join("A/miles.mp3"), b"").unwrap();
        std::fs::write(base.join("B/miles.mp3"), b"").unwrap();
        admin
            .set_data(serde_json::json!({
                "op": "add_source", "kind": "local",
                "path": base.display().to_string(),
                "host": "", "share": "", "user": "", "domain": "",
                "password": "", "writable": false
            }))
            .await
            .unwrap();
        base
    }

    #[tokio::test]
    async fn une_recherche_se_limite_au_dossier_demande() {
        // Signalé à l'usage : la recherche partait toujours de la racine, donc
        // elle ratissait tout le NAS quel que soit le dossier ouvert — lent, et
        // noyé d'homonymes venus d'ailleurs.
        let (mut admin, _) = admin_de_test();
        let base = racine_locale_peuplee(&mut admin).await;
        let nom = admin.roots.read().await.root[0].name.clone();
        admin
            .set_data(serde_json::json!({"op": "search", "root": nom, "path": "A", "query": "miles"}))
            .await
            .unwrap();
        let d = admin.get_data().await;
        let resultats = d["browse"]["results"].as_array().unwrap().clone();
        // Un seul : celui de B est hors du dossier demandé.
        assert_eq!(resultats.len(), 1, "la recherche a debordé du dossier : {resultats:?}");
        // Relatif à la RACINE et non au dossier cherché : c'est cette forme que
        // la page renvoie ensuite dans un `add_file`, et un chemin relatif au
        // sous-dossier y désignerait un fichier inexistant.
        assert_eq!(resultats[0].as_str().unwrap(), "A/miles.mp3");
        assert_eq!(d["browse"]["path"].as_str().unwrap(), "A");
        assert_eq!(d["browse"]["query"].as_str().unwrap(), "miles");
        drop(base);
    }

    #[tokio::test]
    async fn un_parcours_se_distingue_d_une_recherche_par_sa_requete_vide() {
        // Les deux se rangent au même endroit côté plugin. Sans ce marqueur, la
        // page ne peut pas distinguer la réponse à son parcours de celle à une
        // recherche portant sur le même dossier, et remplirait le niveau avec
        // des résultats de recherche.
        let (mut admin, _) = admin_de_test();
        let base = racine_locale_peuplee(&mut admin).await;
        let nom = admin.roots.read().await.root[0].name.clone();
        admin
            .set_data(serde_json::json!({"op": "browse", "root": nom, "path": "A"}))
            .await
            .unwrap();
        let d = admin.get_data().await;
        assert_eq!(d["browse"]["query"].as_str().unwrap(), "");
        drop(base);
    }
```

- [ ] **Étape 2 : les lancer et vérifier qu'ils échouent**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-plugin-files recherche_se_limite requete_vide"
```
Attendu : ÉCHEC. `une_recherche_se_limite_au_dossier_demande` rapporte deux
résultats (`A/miles.mp3` **et** `B/miles.mp3`) et un `path` vide ;
`un_parcours_se_distingue_d_une_recherche_par_sa_requete_vide` échoue sur
`query` absent (`Null` au lieu de `""`).

- [ ] **Étape 3 : implémenter**

Dans l'énumération `Op`, ajouter `path` à `Search` :

```rust
    Search { root: String, #[serde(default)] path: String, query: String },
```

Branche `Op::Browse` — ajouter le marqueur dans la charge rendue, à côté de
`"results": []` :

```rust
                    // Vide, et c'est un marqueur, pas un oubli : la page s'en
                    // sert pour distinguer la réponse à un parcours de celle à
                    // une recherche portant sur le même dossier.
                    "query": "",
```

Branche `Op::Search`, en entier :

```rust
            Op::Search { root, path, query } => {
                // Deux résolutions, deux rôles : `dir` est le dossier où l'on
                // cherche, `base` la racine à laquelle les résultats sont
                // rapportés. Les confondre rendrait des chemins relatifs au
                // sous-dossier, qu'un `add_file` résoudrait ailleurs.
                let dir = self.sous_racine(&root, &path).await?;
                let base = self.sous_racine(&root, "").await?;
                let cat = self.catalog.clone();
                let motif = query.clone();
                let (trouves, tronque) = tokio::task::spawn_blocking(move || {
                    scan::search(&dir, &motif, 200, scan::MAX_VISITES)
                })
                .await
                .map_err(|e| format!("search task: {e}"))?
                .map_err(|e| e.message(&cat.read().unwrap()))?;
                // Chemins **relatifs à la racine** : c'est ce que la page
                // renvoie ensuite dans un `add_file`, et un chemin absolu y
                // serait refusé par la garde d'évasion.
                let relatifs: Vec<String> = trouves
                    .iter()
                    .filter_map(|p| p.strip_prefix(&base).ok())
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .collect();
                *self.browse.lock().unwrap() = serde_json::json!({
                    "root": root,
                    // Le dossier cherché, et non la chaîne vide : la page ne
                    // retient que la réponse à la demande qu'elle vient de
                    // faire, et ce couple (chemin, requête) est ce qui
                    // l'identifie.
                    "path": path,
                    "query": query,
                    "dirs": [],
                    "files": [],
                    "playlists": [],
                    "results": relatifs,
                    // Dit à la page qu'il y en avait davantage, pour qu'elle
                    // invite à affiner plutôt que de présenter une liste
                    // tronquée comme si elle était complète.
                    "truncated": tronque,
                });
                Ok(())
            }
```

Attention : `dir` est déplacé dans la closure `spawn_blocking`, il faut donc que
`base` soit calculé **avant** et resté disponible ; le code ci-dessus le fait.

- [ ] **Étape 4 : tests au vert**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-plugin-files"
```
Attendu : 67 tests verts.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-plugin-files/src/admin.rs
git commit -m "feat(files): la recherche porte sur le dossier ouvert et se nomme"
```

---

### Tâche 6 : le volet Parcourir devient un navigateur de fichiers

**Fichiers :**
- Modifier : `crates/ritornello-plugin-files/ui/src/donnees.ts` (interface
  `Navigation` vers la ligne 89, `normaliserBrowse` vers la ligne 260)
- Remplacer : `crates/ritornello-plugin-files/ui/src/VoletParcourir.vue`
- Remplacer : `crates/ritornello-plugin-files/ui/src/VoletParcourir.test.ts`
- Modifier : `crates/ritornello-plugin-files/ui/src/harnais.ts` (interface
  `Navigue`, `CATALOGUE`)
- Modifier : `crates/ritornello-plugin-files/src/locales/en.toml` et
  `deploy/locales/files/fr.toml`
- Test : `crates/ritornello-plugin-files/ui/src/donnees.test.ts` (le champ
  `query`)

**Interfaces :**
- Consomme : `Op::Search { root, path, query }` et le champ `query` de la charge
  `browse` (tâche 5) ; `tronquerDebut(v: string): string` déjà exporté par
  `donnees.ts` ; `Entree { name, path, dir, playlist }`.
- Produit : `Navigation.query: string`. Attributs de test du volet :
  `data-browse-root`, `data-browse-up`, `data-browse-path`, `data-add-current`,
  `data-browse-list`, `data-browse-row`, `data-browse-dir`, `data-browse-name`,
  `data-browse-empty`, `data-add-dir`, `data-add-file`, `data-load-m3u`,
  `data-search-query`, `data-search`, `data-search-scope`,
  `data-search-results`, `data-search-row`, `data-search-truncated`,
  `data-no-results`, `data-add-result`. **Disparaissent** : `data-tree`,
  `data-tree-row`, `data-tree-toggle`, `data-tree-name`, `data-tree-empty`.

- [ ] **Étape 1 : deux clés i18n et le champ `query`**

Dans `crates/ritornello-plugin-files/src/locales/en.toml`, ajouter :

```toml
btn_add_current_folder = "Add this folder"
search_scope = "The search covers {path}"
```

Dans `deploy/locales/files/fr.toml` :

```toml
btn_add_current_folder = "Ajouter ce dossier"
search_scope = "La recherche porte sur {path}"
```

Retirer des **deux** catalogues les clés `btn_expand` et `btn_collapse`, que
plus rien n'emploie une fois l'arbre remplacé — vérifier d'abord par
`grep -rn "btn_expand\|btn_collapse" crates web deploy` qu'aucun autre appelant
ne subsiste. Les retirer aussi de `CATALOGUE` dans `harnais.ts`, et y ajouter :

```ts
  btn_add_current_folder: 'Ajouter ce dossier',
  search_scope: 'La recherche porte sur {path}',
```

Dans `donnees.ts`, ajouter le champ à `Navigation`, avec son commentaire :

```ts
  /**
   * Motif de la dernière recherche, vide pour un parcours.
   *
   * Ce que la page en fait : distinguer la réponse à SON parcours de celle à
   * une recherche portant sur le même dossier — les deux se rangent au même
   * endroit côté plugin.
   */
  query: string
```

et le remplir dans `normaliserBrowse`, à côté de `path` :

```ts
    query: chaine(o.query),
```

Dans `harnais.ts`, ajouter à l'interface `Navigue` :

```ts
  /** Vide pour un parcours, le motif pour une recherche. */
  query?: string
```

et à l'objet `browse` du défaut de `etat()` : `query: ''`.

Test à ajouter dans `donnees.test.ts`, près des autres cas de
`normaliserBrowse` :

```ts
  it('retient le motif de recherche, vide pour un parcours', () => {
    expect(normaliserBrowse({ root: 'nas', path: 'A', query: 'miles' }).query).toBe('miles')
    expect(normaliserBrowse({ root: 'nas', path: 'A' }).query).toBe('')
  })
```

- [ ] **Étape 2 : écrire les tests du volet, qui échouent**

**Remplacer entièrement** `VoletParcourir.test.ts` par :

```ts
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'
import { BASE, CATALOGUE, serveur } from './harnais'

/** Un niveau tel que `scan::list_dir` le rend : des **noms**, pas des chemins. */
interface Niveau {
  dirs: string[]
  files: string[]
  /** Fichiers `.m3u`/`.m3u8` : ils se chargent, ils ne s'ajoutent pas. */
  playlists?: string[]
}

/**
 * Simulacre d'un partage : le plugin ne rend qu'**un** niveau par `browse`,
 * celui qu'on lui demande, et il range parcours et recherche au même endroit.
 * `query` est ce qui les distingue.
 */
function arbre(niveaux: Record<string, Niveau>, trouvailles: string[] = [], tronque = false) {
  const s = serveur({ roots: [{ name: 'nas', kind: 'smb', host: 'h', share: 'musique' }] })
  s.surPut = (charge) => {
    const chemin = String(charge.path ?? '')
    if (charge.op === 'browse') {
      const n = niveaux[chemin] ?? { dirs: [], files: [] }
      s.data.browse = {
        root: 'nas',
        path: chemin,
        query: '',
        dirs: n.dirs,
        files: n.files,
        playlists: n.playlists ?? [],
        results: [],
      }
    }
    if (charge.op === 'search') {
      s.data.browse = {
        root: 'nas',
        path: chemin,
        query: String(charge.query ?? ''),
        dirs: [],
        files: [],
        results: trouvailles,
        truncated: tronque,
      }
    }
  }
  return s
}

const NIVEAUX: Record<string, Niveau> = {
  '': { dirs: ['Albums'], files: ['jingle.mp3'] },
  Albums: { dirs: ['Jazz'], files: ['01.mp3'], playlists: ['tout.m3u'] },
  'Albums/Jazz': { dirs: [], files: ['Kind of Blue.flac'] },
}

async function monterArbre(trouvailles: string[] = [], tronque = false) {
  const s = arbre(NIVEAUX, trouvailles, tronque)
  const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
  await flushPromises()
  return { w, s }
}

/** Noms affichés dans le niveau courant, dossiers comme fichiers. */
function noms(w: ReturnType<typeof mount>): string[] {
  return w.findAll('[data-browse-name]').map((n) => n.text())
}

describe('navigateur de fichiers', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('n’ouvre qu’un niveau au chargement de la page', async () => {
    // Régression encodée : demander l'arborescence entière d'un partage de
    // plusieurs dizaines de milliers de fichiers dépasserait de loin le plafond
    // de 5 s du cœur — la page n'afficherait rien du tout.
    const { w, s } = await monterArbre()
    expect(s.putsDe('browse')).toEqual([{ op: 'browse', root: 'nas', path: '' }])
    expect(noms(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('descend dans un dossier et REMPLACE le niveau affiché', async () => {
    // Un navigateur, pas un arbre : c'est ce qui borne la hauteur de la liste.
    // Le chemin envoyé est recomposé — `Albums/Jazz` et non `Jazz`, que le
    // plugin résoudrait contre la racine, donc ailleurs.
    const { w, s } = await monterArbre()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(s.putsDe('browse').map((b) => b.path)).toEqual(['', 'Albums'])
    expect(noms(w)).toEqual(['Jazz', 'tout.m3u', '01.mp3'])
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(s.putsDe('browse').map((b) => b.path)).toEqual(['', 'Albums', 'Albums/Jazz'])
    expect(noms(w)).toEqual(['Kind of Blue.flac'])
  })

  it('remonte au parent, et ne le propose pas au sommet', async () => {
    const { w, s } = await monterArbre()
    expect(w.find('[data-browse-up]').attributes('disabled')).toBeDefined()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(w.find('[data-browse-up]').attributes('disabled')).toBeUndefined()
    await w.find('[data-browse-up]').trigger('click')
    await flushPromises()
    expect(s.putsDe('browse').map((b) => b.path)).toEqual(['', 'Albums', ''])
    expect(noms(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('affiche le chemin ouvert, racine comprise', async () => {
    // Sans le nom de la racine, un chemin relatif ne dit pas où l'on se trouve
    // quand plusieurs sources sont déclarées.
    const { w } = await monterArbre()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    expect(w.find('[data-browse-path]').attributes('title')).toBe('nas/Albums')
  })

  it('ajoute le dossier ouvert, sauf au sommet', async () => {
    // Au sommet le geste existe déjà sur la ligne de la source (volet Sources) :
    // deux boutons pour le même effet faisaient chercher une différence qui
    // n'existait pas.
    const { w, s } = await monterArbre()
    expect(w.find('[data-add-current]').exists()).toBe(false)
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    await w.find('[data-add-current]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_dir')).toEqual([{ op: 'add_dir', root: 'nas', path: 'Albums' }])
  })

  it('ajoute un dossier listé de façon récursive, et un fichier seul', async () => {
    const { w, s } = await monterArbre()
    await w.find('[data-add-dir]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_dir')).toEqual([{ op: 'add_dir', root: 'nas', path: 'Albums' }])
    await w.find('[data-add-file]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_file')).toEqual([{ op: 'add_file', root: 'nas', path: 'jingle.mp3' }])
  })

  it('un m3u se **charge**, il ne s’ajoute pas', async () => {
    // L'action est délibérément différente de celle des pistes : une liste
    // remplace la liste en cours. Les confondre ferait ajouter un fichier texte
    // que mpv tenterait de jouer.
    const { w, s } = await monterArbre()
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    const rangees = w.findAll('[data-browse-row]')
    const rangeeM3u = rangees.find((r) => r.find('[data-browse-name]').text() === 'tout.m3u')
    expect(rangeeM3u).toBeDefined()
    expect(rangeeM3u!.find('[data-add-file]').exists()).toBe(false)
    await rangeeM3u!.find('[data-load-m3u]').trigger('click')
    await flushPromises()
    expect(s.putsDe('load_m3u')).toEqual([
      { op: 'load_m3u', root: 'nas', path: 'Albums/tout.m3u' },
    ])
    expect(s.putsDe('add_file')).toEqual([])
  })

  it('cherche dans le dossier ouvert, sans effacer le niveau affiché', async () => {
    // Les deux vivent au même endroit côté plugin : si la page lisait le niveau
    // dans la réponse, une recherche viderait la liste sous les yeux.
    const { w, s } = await monterArbre(['Albums/Jazz/miles.flac'])
    await w.find('[data-browse-dir]').trigger('click')
    await flushPromises()
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(s.putsDe('search')).toEqual([
      { op: 'search', root: 'nas', path: 'Albums', query: 'miles' },
    ])
    // Le chemin complet, pas seulement le nom : une recherche rapporte des
    // homonymes venus de dossiers différents, et rien d'autre ne les distingue.
    expect(w.find('[data-search-row]').text()).toContain('Albums/Jazz/miles.flac')
    expect(noms(w)).toContain('Jazz')
  })

  it('dit sur quel dossier la recherche porte', async () => {
    const { w } = await monterArbre()
    expect(w.find('[data-search-scope]').text()).toContain('nas')
  })

  it('ajoute un résultat de recherche par son chemin', async () => {
    const { w, s } = await monterArbre(['Albums/Jazz/miles.flac'])
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    await w.find('[data-add-result]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_file')).toEqual([
      { op: 'add_file', root: 'nas', path: 'Albums/Jazz/miles.flac' },
    ])
  })

  it('signale une recherche tronquée au lieu de la présenter comme complète', async () => {
    // Régression encodée : `scan::search` plafonne à 200 résultats et le dit
    // par `truncated`. Sans cette phrase, l'utilisateur qui ne voit pas son
    // fichier conclut qu'il n'est pas là.
    const { w } = await monterArbre(['Albums/Jazz/miles.flac'], true)
    await w.find('[data-search-query]').setValue('a')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.find('[data-search-truncated]').text()).toContain('affinez')
  })

  it('une recherche vide n’émet rien', async () => {
    const { w, s } = await monterArbre()
    await w.find('[data-search-query]').setValue('   ')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(s.putsDe('search')).toHaveLength(0)
  })

  it('ne prend pas une réponse de recherche pour un niveau', async () => {
    // Garde-fou du marqueur `query` : sans lui, la réponse d'une recherche
    // portant sur le dossier ouvert remplirait le niveau avec ses résultats,
    // c'est-à-dire avec rien du tout (`dirs` et `files` y sont vides).
    const { w } = await monterArbre(['Albums/Jazz/miles.flac'])
    await w.find('[data-search-query]').setValue('miles')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(noms(w)).toEqual(['Albums', 'jingle.mp3'])
  })

  it('un refus de parcours ne fait pas passer le dossier pour vide', async () => {
    // Mémoriser un niveau vide après un refus le ferait passer pour un dossier
    // vide, et l'utilisateur n'aurait aucun moyen de réessayer sans recharger
    // la page.
    const s = arbre(NIVEAUX)
    s.refus = 'could not read "Albums": the share may be unreachable'
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    expect(w.find('[data-message]').text()).toBe(s.refus)
    expect(w.find('[data-browse-empty]').exists()).toBe(false)
  })

  it('sans racine déclarée, le volet le dit au lieu d’émettre un parcours', async () => {
    const s = serveur()
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    expect(s.putsDe('browse')).toHaveLength(0)
    // Même phrase que le volet Sources : deux formulations pour le même vide
    // laisseraient croire à deux causes différentes.
    expect(w.find('[data-volet-parcourir]').text()).toContain('Aucune source déclarée')
  })
})
```

- [ ] **Étape 3 : les lancer et vérifier qu'ils échouent**

`npx vitest run src/VoletParcourir.test.ts src/donnees.test.ts` depuis
`crates/ritornello-plugin-files/ui`
Attendu : ÉCHEC en masse — aucun `[data-browse-*]` n'existe encore.

- [ ] **Étape 4 : implémenter le volet**

**Remplacer entièrement** `VoletParcourir.vue` par :

```vue
<script setup lang="ts">
import { Button, Input } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { tronquerDebut, type Donnees, type Entree, type Envoyer, type T } from './donnees'

/**
 * Le navigateur de fichiers d'une source déclarée.
 *
 * Un seul niveau à l'écran, et non un arbre qu'on déplie : sur une
 * bibliothèque réelle, l'arbre déplié devenait plus haut que la page et le
 * geste utile — descendre — se perdait dans les rangées des niveaux
 * précédents. Même forme que l'assistant de déclaration (`ChoixDossier`), à
 * ceci près qu'ici les fichiers sont montrés, et pas seulement les dossiers.
 */
const props = defineProps<{ donnees: Donnees; t: T; envoyer: Envoyer; fige: boolean }>()

/** Chemin du niveau supérieur d'une racine : la chaîne vide, comme côté plugin. */
const SOMMET = ''

const racine = ref('')
/** Dossier ouvert, relatif à la racine. */
const chemin = ref(SOMMET)
/**
 * Contenu du dossier ouvert.
 *
 * Mémorisé ici plutôt que lu directement dans `donnees.browse` : le plugin
 * range parcours **et** recherche au même endroit, donc une recherche viderait
 * la liste sous les yeux de l'utilisateur. `null` tant que rien n'a abouti —
 * ce qui n'est pas la même chose qu'un dossier vide.
 */
const entrees = ref<Entree[] | null>(null)
const query = ref('')
const resultats = ref<Entree[] | null>(null)
const tronque = ref(false)

function estOuverte(nom: string): boolean {
  return props.donnees.roots.some((r) => r.name === nom)
}

/** Change de racine ou de dossier : ce qui était affiché ne parle plus du bon. */
function reinitialiser(): void {
  chemin.value = SOMMET
  entrees.value = null
  resultats.value = null
  tronque.value = false
  query.value = ''
}

watch(
  // Un nom de racine ne peut contenir ni espace ni virgule (`champ_sur`, côté
  // plugin) : les joindre par un espace donne bien une empreinte injective.
  () => props.donnees.roots.map((r) => r.name).join(' '),
  () => {
    // La racine choisie a pu disparaître d'un enregistrement à l'autre : sans
    // ce recalage, le volet continuerait d'adresser ses `browse` à un nom que
    // le plugin ne connaît plus, et n'afficherait que des refus.
    if (estOuverte(racine.value)) return
    racine.value = props.donnees.roots[0]?.name ?? ''
    reinitialiser()
    if (racine.value) void charger(SOMMET)
  },
  { immediate: true },
)

function changerRacine(nom: string): void {
  if (nom === racine.value) return
  racine.value = nom
  reinitialiser()
  void charger(SOMMET)
}

async function charger(cible: string): Promise<void> {
  if (!racine.value) return
  const etat = await props.envoyer({ op: 'browse', root: racine.value, path: cible })
  // Refus : on ne mémorise rien. Mémoriser un niveau vide le ferait passer
  // pour un dossier vide, et l'utilisateur n'aurait aucun moyen de réessayer
  // sans recharger la page.
  if (!etat) return
  const nav = etat.browse
  // On n'accepte que la réponse à la demande qu'on vient de faire : parcours et
  // recherche se rangent au même endroit côté plugin, et une réponse en retard
  // viendrait remplir le mauvais niveau. `query` vide est ce qui distingue un
  // parcours d'une recherche portant sur le même dossier.
  if (nav.root !== racine.value || nav.path !== cible || nav.query !== '') return
  chemin.value = cible
  entrees.value = nav.entrees
}

function descendre(nom: string): void {
  void charger(chemin.value ? `${chemin.value}/${nom}` : nom)
}

function remonter(): void {
  if (!chemin.value) return
  void charger(chemin.value.replace(/\/?[^/]+$/, ''))
}

/**
 * Adresse du dossier ouvert, nom de la racine compris.
 *
 * Le chemin du plugin est relatif à la racine : affiché seul, il ne dit pas
 * dans laquelle on se trouve dès que plusieurs sources sont déclarées.
 */
const cheminAffiche = computed(() => [racine.value, chemin.value].filter(Boolean).join('/'))
/** Tronqué **par le début** : sur un chemin, l'information utile est la fin. */
const cheminCourt = computed(() => tronquerDebut(cheminAffiche.value))

async function chercher(): Promise<void> {
  const q = query.value.trim()
  if (!q) {
    resultats.value = null
    return
  }
  const cible = chemin.value
  const etat = await props.envoyer({ op: 'search', root: racine.value, path: cible, query: q })
  if (!etat) return
  const nav = etat.browse
  if (nav.root !== racine.value || nav.path !== cible || nav.query !== q) return
  resultats.value = nav.resultats
  // Le plugin plafonne la recherche : sans ce drapeau, une liste tronquée
  // passerait pour complète et l'utilisateur conclurait que son fichier n'est
  // pas là.
  tronque.value = nav.tronque
}

function ajouterDossier(cible: string): void {
  // Récursif et **asynchrone** côté plugin : la réponse n'attend pas la fin du
  // balayage, c'est le sondage de la page qui en montre l'avancement.
  void props.envoyer({ op: 'add_dir', root: racine.value, path: cible })
}

function ajouterFichier(cible: string): void {
  void props.envoyer({ op: 'add_file', root: racine.value, path: cible })
}

/**
 * Charge un m3u trouvé en parcourant : il **remplace** la liste en cours.
 *
 * Distinct de la liste déroulante des listes *enregistrées* du volet Liste :
 * celle-ci va chercher un nom dans un magasin, tandis qu'ici on désigne un
 * fichier par son chemin, là où il se trouve sur la source.
 */
function chargerListe(cible: string): void {
  void props.envoyer({ op: 'load_m3u', root: racine.value, path: cible })
}
</script>

<template>
  <section class="space-y-3" data-volet-parcourir>
    <h2 class="font-medium">{{ t('browse_title') }}</h2>

    <p v-if="!donnees.roots.length" class="text-sm text-muted-foreground">
      {{ t('no_sources') }}
    </p>

    <template v-else>
      <div class="flex flex-wrap items-center gap-2">
        <label class="text-sm text-muted-foreground" for="racine-parcourue">
          {{ t('root_label') }}
        </label>
        <select
          id="racine-parcourue"
          data-browse-root
          class="h-9 rounded-md border border-input bg-transparent px-2 text-sm"
          :value="racine"
          :disabled="fige"
          @change="changerRacine(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="r in donnees.roots" :key="r.name" :value="r.name">{{ r.name }}</option>
        </select>
      </div>

      <!-- `min-w-0` partout où du texte long descend : la largeur minimale d'un
           enfant de flex vaut par défaut celle de son contenu, et un chemin long
           pousserait la rangée hors du cadre. C'est aussi ce qui rend `truncate`
           opérant. -->
      <div class="flex min-w-0 items-center gap-2 text-sm">
        <Button
          variant="ghost"
          size="sm"
          class="shrink-0"
          data-browse-up
          :disabled="fige || !chemin"
          @click="remonter"
        >
          ↑ {{ t('btn_up') }}
        </Button>
        <span
          class="min-w-0 flex-1 truncate text-muted-foreground"
          data-browse-path
          :title="cheminAffiche"
        >
          {{ cheminCourt }}
        </span>
        <!-- Absent au sommet : ajouter la source entière vit sur la ligne de la
             source, dans le volet Sources. Deux boutons pour le même effet
             faisaient chercher une différence qui n'existait pas. -->
        <Button
          v-if="chemin"
          variant="secondary"
          size="sm"
          data-add-current
          :disabled="fige"
          @click="ajouterDossier(chemin)"
        >
          {{ t('btn_add_current_folder') }}
        </Button>
      </div>

      <!-- Hauteur bornée et défilement propre : c'est tout l'objet du
           navigateur. Un dossier de mille fichiers ne doit pas repousser la
           recherche et le reste de la page hors de l'écran. -->
      <ul class="max-h-96 min-w-0 space-y-1 overflow-y-auto text-sm" data-browse-list>
        <li
          v-for="e in entrees ?? []"
          :key="`${e.dir ? 'd' : 'f'}:${e.path}`"
          data-browse-row
          class="flex min-w-0 items-center gap-2"
        >
          <template v-if="e.dir">
            <button
              type="button"
              data-browse-dir
              class="min-w-0 flex-1 truncate rounded px-2 py-1 text-left hover:bg-accent"
              :disabled="fige"
              :title="e.name"
              @click="descendre(e.name)"
            >
              <span aria-hidden="true" class="mr-1">📁</span
              ><span data-browse-name>{{ e.name }}</span>
            </button>
            <Button
              variant="secondary"
              size="sm"
              data-add-dir
              :disabled="fige"
              @click="ajouterDossier(e.path)"
            >
              {{ t('btn_add_to_playlist') }}
            </Button>
          </template>
          <!-- Une liste de lecture porte une action **différente** : elle
               remplace la liste en cours au lieu de s'y ajouter. Les confondre
               ferait ajouter un fichier texte que mpv tenterait de jouer. -->
          <template v-else-if="e.playlist">
            <span class="min-w-0 flex-1 truncate px-2">
              <span aria-hidden="true" class="mr-1">☰</span
              ><span data-browse-name>{{ e.name }}</span>
            </span>
            <Button
              variant="secondary"
              size="sm"
              data-load-m3u
              :disabled="fige"
              @click="chargerListe(e.path)"
            >
              {{ t('btn_load_m3u') }}
            </Button>
          </template>
          <template v-else>
            <span class="min-w-0 flex-1 truncate px-2" data-browse-name>{{ e.name }}</span>
            <Button
              variant="ghost"
              size="sm"
              data-add-file
              :disabled="fige"
              @click="ajouterFichier(e.path)"
            >
              {{ t('btn_add_to_playlist') }}
            </Button>
          </template>
        </li>
        <!-- `entrees` non nul, donc un niveau a bien été rapporté : un dossier
             réellement vide, et non un parcours qui n'a pas abouti. -->
        <li
          v-if="entrees && !entrees.length"
          class="px-2 text-muted-foreground"
          data-browse-empty
        >
          {{ t('empty_folder') }}
        </li>
      </ul>

      <!-- La recherche vit **sous** le dossier ouvert, parce qu'elle porte sur
           lui : la placer au-dessus laissait croire qu'elle ratissait toute la
           source. -->
      <div class="flex flex-wrap items-center gap-2">
        <Input
          v-model="query"
          data-search-query
          class="min-w-48 flex-1"
          :placeholder="t('search_placeholder')"
          @keydown.enter="chercher"
        />
        <Button data-search :disabled="fige" @click="chercher">{{ t('btn_search') }}</Button>
      </div>
      <p class="text-xs text-muted-foreground" data-search-scope>
        {{ t('search_scope', { path: cheminAffiche }) }}
      </p>

      <div v-if="resultats" class="space-y-1" data-search-results>
        <p v-if="!resultats.length" class="text-sm text-muted-foreground" data-no-results>
          {{ t('no_results') }}
        </p>
        <!-- Le plafond du plugin est silencieux dans la liste : sans cette
             phrase, une recherche tronquée passerait pour complète et
             l'utilisateur conclurait que son fichier n'est pas là. -->
        <p v-if="tronque" class="text-sm text-muted-foreground" data-search-truncated>
          {{ t('search_truncated', { count: resultats.length }) }}
        </p>
        <div
          v-for="e in resultats"
          :key="`${e.dir ? 'd' : 'f'}:${e.path}`"
          class="flex min-w-0 items-center gap-2 text-sm"
          data-search-row
        >
          <!-- Le chemin complet, pas seulement le nom : une recherche rapporte
               des homonymes de dossiers différents, et rien d'autre ne permet
               de les distinguer. -->
          <span class="min-w-0 flex-1 truncate">{{ e.path }}</span>
          <Button
            variant="secondary"
            size="sm"
            data-add-result
            :disabled="fige"
            @click="e.dir ? ajouterDossier(e.path) : ajouterFichier(e.path)"
          >
            {{ t('btn_add_to_playlist') }}
          </Button>
        </div>
      </div>
    </template>
  </section>
</template>
```

- [ ] **Étape 5 : tests au vert**

`npx vitest run` depuis `crates/ritornello-plugin-files/ui`
Attendu : tout vert. Puis le typage : `npx vue-tsc --noEmit` depuis le même
répertoire — aucune erreur.

Si `[data-search-scope]` ne contient pas « nas », vérifier que `search_scope` a
bien été ajouté à `CATALOGUE` du harnais : sans cela `createT` rend la clé brute.

- [ ] **Étape 6 : commit**

```bash
git add crates/ritornello-plugin-files/ui/src crates/ritornello-plugin-files/src/locales/en.toml deploy/locales/files/fr.toml
git commit -m "feat(files): le volet Parcourir devient un navigateur de fichiers"
```

---

### Tâche 7 : parcours de bout en bout, paquet reconstruit

**Fichiers :**
- Modifier : `web/app/e2e/files.spec.ts` (les étapes du volet Parcourir, vers
  les lignes 100-130 et 165-175)
- Reconstruire (sans committer) : `crates/ritornello-plugin-files/ui/dist/ui.js`
  et `dist/ui.css`. Ils ne sont **pas** suivis par git, mais `admin.rs` les
  embarque par `include_str!` : ils doivent exister sur le disque pour que
  `cargo build` aboutisse et pour que le parcours e2e serve l'IHM à jour.

**Interfaces :**
- Consomme : tous les attributs `data-browse-*` produits par la tâche 6.
- Produit : rien.

- [ ] **Étape 1 : adapter le parcours**

Dans `web/app/e2e/files.spec.ts`, remplacer les sélecteurs de l'arbre par ceux
du navigateur. Les correspondances :

- `[data-tree-row]` → `[data-browse-row]`
- `[data-tree-name]` → `[data-browse-name]`
- un clic sur `[data-tree-toggle]` pour **entrer** dans un dossier → un clic sur
  `[data-browse-dir]` de la rangée voulue
- après être entré, le niveau **remplace** le précédent : les assertions qui
  comptaient les rangées des deux niveaux à la fois (par exemple
  `toHaveText(['Album', 'tout.m3u', 'piste.mp3'])` sur l'arbre déplié) doivent
  ne décrire que le contenu du dossier ouvert.

Relire tout le fichier à la recherche de `data-tree` avant de conclure ; il ne
doit plus en rester aucune occurrence.

- [ ] **Étape 2 : reconstruire le paquet du plugin**

Depuis `crates/ritornello-plugin-files/ui` :

```
npm run build
```

Attendu : `vite build` puis `verifier-dist-plugin.mjs` sans erreur, et
`dist/ui.js` modifié. En cas d'échec sur une dépendance introuvable, vérifier
que les jonctions du worktree existent (voir « Contraintes globales ») — et ne
**jamais** créer de jonction pour `vite` lui-même : deux instances
coexisteraient et tous les `.vue` remonteraient « invalid JS syntax ».

- [ ] **Étape 3 : compiler et lancer le parcours**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo build --workspace"
```

puis, depuis `web/app` :

```
npx playwright test e2e/files.spec.ts
```

Attendu : le parcours passe. Il dure plusieurs dizaines de secondes (fixtures
audio encodées au démarrage, balayage sondé à la seconde). En cas d'échec, lire
le message de Playwright avant de toucher au code : le harnais publie la racine
des fixtures dans `target/e2e-etat.json`, et un échec précoce vient souvent de
là plutôt que de l'IHM.

- [ ] **Étape 4 : vérification d'ensemble**

Les trois suites, dans cet ordre :

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test --workspace"
```
puis depuis `crates/ritornello-plugin-files/ui` : `npx vitest run`
puis depuis `web/app` : `npx vitest run`

Attendu : tout vert. Rapporter les chiffres exacts.

- [ ] **Étape 5 : commit**

Le `dist/` reconstruit n'entre pas dans le commit : il n'est pas suivi par git,
et `git add` sur un chemin ignoré échouerait. Seul l'e2e est commité.

```bash
git add web/app/e2e/files.spec.ts
git commit -m "test(files): le parcours e2e navigue au lieu de deplier"
```

---

### Tâche 8 : le 502 d'un plugin porte sa cause, et le délai se distingue de la panne

Ajoutée en cours de chantier. Symptôme signalé à l'usage : en déclarant un
partage depuis un poste en wifi faible, la page affichait `HTTP 502`, un code
nu. La cause était pourtant connue une ligne plus haut — `admin plugin: request
timeout`, le plafond de 5 s du protocole d'admin — mais elle partait dans les
journaux, pas à l'écran. Le lien avec le wifi est cohérent avec les options de
montage du dépôt (`mount_options.rs` borne la suite par `soft`,
`echo_interval=10`, `retrans=1`, mais **pas** l'établissement initial de la
session SMB) ; il n'est pas mesuré sur l'appareil, et cette tâche ne prétend
pas le corriger — elle rend la panne lisible.

**Fichiers :**
- Modifier : `crates/ritornello-plugin-sdk/src/client.rs` (`AdminClient::request`,
  vers la ligne 220) et le `pub use` qui expose `AdminClient` (voir le `lib.rs`
  du SDK)
- Modifier : `crates/ritornello-core/src/admin.rs` (quatre sites : `admin_asset`
  ligne ~73, `admin_i18n` ligne ~102, `admin_get_data` ligne ~115,
  `admin_put_data` ligne ~133)
- Modifier : `crates/ritornello-core/src/locales/en.toml` et
  `deploy/locales/core/fr.toml`
- Test : `crates/ritornello-core/src/admin.rs`, `mod tests` (le faux
  `Fake` ligne ~151 et le test `plugin_injoignable_renvoie_502` ligne ~373)

**Interfaces :**
- Produit : `ritornello_plugin_sdk::AdminIpcError { Timeout, Closed }`, une
  erreur typée qui implémente `std::error::Error` et dont le `Display` reste en
  **anglais** (elle part dans les journaux ; l'écran reçoit une phrase du
  catalogue).
- Produit : deux clés du catalogue **du cœur**, `plugin_unreachable` et
  `plugin_timeout`.
- Consomme : `AppState::catalog` (déjà présent, `status.rs:33`), le motif
  `(StatusCode, Json({"error": msg}))` déjà employé par `system.rs:679` pour un
  502.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `mod tests` de `crates/ritornello-core/src/admin.rs`, ajouter un drapeau au
faux backend. Il porte déjà `down: bool` (une panne quelconque) ; il lui faut
`lent: bool`, qui produit l'erreur **typée** du SDK :

```rust
    #[derive(Default)]
    struct Fake {
        reject: bool,
        down: bool,
        /// Le plugin répond, mais au-delà du plafond de 5 s. Distinct de `down`
        /// justement parce que le message rendu doit l'être aussi.
        lent: bool,
        appels_asset: Arc<std::sync::atomic::AtomicUsize>,
    }
```

et, dans chacune des quatre méthodes de `impl AdminBackend for Fake`, ajouter
la branche avant celle de `down` — par exemple pour `get_data` :

```rust
        async fn get_data(&self) -> Result<serde_json::Value> {
            if self.lent { return Err(ritornello_plugin_sdk::AdminIpcError::Timeout.into()) }
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "stations": [] }))
        }
```

Remplacer le test `plugin_injoignable_renvoie_502` par ces deux-là :

```rust
    #[tokio::test]
    async fn un_plugin_injoignable_dit_pourquoi_au_lieu_dun_code_nu() {
        // Symptôme signalé : l'écran affichait « HTTP 502 ». Le client web ne
        // sait lire que `{"error": …}` ; un corps en texte brut le faisait
        // retomber sur le code, alors que la cause était connue.
        let app = router(state_with(Fake { down: true, ..Default::default() }));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let corps = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&corps).expect("corps JSON");
        let msg = json["error"].as_str().expect("champ error");
        // Une phrase, pas une clé de catalogue : le repli clé par clé de
        // `Catalog::get` est silencieux, et une clé nue s'afficherait telle
        // quelle.
        assert!(msg.contains(' '), "cle brute renvoyee a l'ecran : {msg}");
    }

    #[tokio::test]
    async fn un_plugin_trop_lent_ne_se_dit_pas_injoignable() {
        // Deux pannes distinctes, deux conduites à tenir : un plugin mort
        // appelle un redémarrage, un plugin trop lent envoie regarder le
        // réseau. Le cœur les aplatissait en un seul message.
        let lent = router(state_with(Fake { lent: true, ..Default::default() }));
        let r1 = lent
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::BAD_GATEWAY);
        let c1 = r1.into_body().collect().await.unwrap().to_bytes();
        let m1 = serde_json::from_slice::<serde_json::Value>(&c1).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();

        let mort = router(state_with(Fake { down: true, ..Default::default() }));
        let r2 = mort
            .oneshot(Request::get("/plugins/radio/api/data").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let c2 = r2.into_body().collect().await.unwrap().to_bytes();
        let m2 = serde_json::from_slice::<serde_json::Value>(&c2).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();

        assert_ne!(m1, m2, "le delai depasse et la panne rendent le meme message");
        assert!(m1.contains(' ') && m2.contains(' '), "cle brute : {m1} / {m2}");
    }
```

- [ ] **Étape 2 : les lancer et vérifier qu'ils échouent**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test -p ritornello-core plugin_"
```
Attendu : ÉCHEC de compilation (`AdminIpcError` n'existe pas). Après l'étape 3,
relancer : les tests doivent alors échouer sur le **corps** — le 502 rend du
texte brut, donc `serde_json::from_slice` panique sur « corps JSON ». C'est
l'échec recherché.

- [ ] **Étape 3 : typer la panne, côté SDK**

Dans `crates/ritornello-plugin-sdk/src/client.rs`, ajouter près de
`AdminClient` :

```rust
/// Panne du dialogue d'admin avec un plugin, **typée** pour que le cœur puisse
/// la distinguer.
///
/// Une chaîne ne suffisait pas : le cœur aplatissait tout en « plugin
/// injoignable », si bien qu'un plugin mort et un plugin qui répond trop
/// lentement recevaient le même message — le premier appelle un redémarrage, le
/// second envoie regarder le réseau.
///
/// Les libellés restent en **anglais** : ils partent dans les journaux, comme
/// tous les messages de ce crate. Ce qui atteint l'écran vient du catalogue du
/// cœur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminIpcError {
    /// Le plafond de 5 s a été atteint : le plugin vit, mais répond trop tard.
    Timeout,
    /// Le socket est tombé, ou la requête a été drainée par une déconnexion.
    Closed,
}

impl std::fmt::Display for AdminIpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Formulations inchangées : elles sont déjà dans les journaux des
            // appareils en service, et les changer casserait toute recherche
            // portant dessus.
            Self::Timeout => write!(f, "admin plugin: request timeout"),
            Self::Closed => write!(f, "admin plugin: response dropped"),
        }
    }
}

impl std::error::Error for AdminIpcError {}
```

et remplacer la fin de `AdminClient::request` :

```rust
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(AdminIpcError::Closed.into()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(AdminIpcError::Timeout.into())
            }
        }
```

Exporter le type là où `AdminClient` l'est déjà : ouvrir le `lib.rs` du SDK,
repérer le `pub use client::{…}` (ou équivalent) et y ajouter `AdminIpcError`.
Ne change **pas** `SourceClient::request` : la moitié « source » n'a pas de
route HTTP derrière elle, et l'élargir sortirait du périmètre.

- [ ] **Étape 4 : faire porter la cause par le 502, côté cœur**

Dans `crates/ritornello-core/src/admin.rs`, ajouter une fonction unique :

```rust
/// Réponse à une panne du dialogue d'admin avec un plugin.
///
/// En un seul endroit parce que les quatre routes d'admin faisaient la même
/// chose de la même façon fautive : journaliser la cause, puis renvoyer un 502
/// dont le corps était le texte brut « plugin injoignable ». Le client web ne
/// lit que `{"error": …}` ; un corps en texte brut le faisait retomber sur
/// « HTTP 502 », un code nu à l'écran pour une panne dont la cause était connue
/// une ligne plus haut.
async fn refus_plugin(st: &AppState, name: &str, contexte: &str, e: &anyhow::Error) -> Response {
    // Le journal garde la cause **entière** et en anglais : c'est elle qui sert
    // au diagnostic à distance, et elle est souvent plus précise que la phrase
    // affichée.
    tracing::warn!("plugin {name} admin unreachable ({contexte}): {e}");
    let cle = match e.downcast_ref::<ritornello_plugin_sdk::AdminIpcError>() {
        // Vivant mais trop lent : dire « injoignable » enverrait redémarrer un
        // processus qui tourne, au lieu de regarder le réseau.
        Some(ritornello_plugin_sdk::AdminIpcError::Timeout) => "plugin_timeout",
        _ => "plugin_unreachable",
    };
    let msg = st.catalog.read().await.get(cle).to_string();
    (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": msg }))).into_response()
}
```

Puis remplacer les quatre sites. Ils suivent tous ce patron — seul le contexte
journalisé change, et il doit rester celui qui y figure déjà (`asset {fichier}`,
`catalog`, `get_data`, `set_data`) :

```rust
            Err(e) => return refus_plugin(&st, &name, &format!("asset {fichier}"), &e).await,
```
```rust
            Err(e) => refus_plugin(&st, &name, "catalog", &e).await,
```
```rust
            Err(e) => refus_plugin(&st, &name, "get_data", &e).await,
```
```rust
            Err(e) => refus_plugin(&st, &name, "set_data", &e).await,
```

Ne touche **pas** aux `(StatusCode::NOT_FOUND, "plugin inconnu")` ni à
`"actif inconnu"` : ce sont des erreurs de routage, sans cause à porter, et les
élargir sortirait du périmètre.

Les deux clés, dans les **deux** catalogues du cœur (un test de parité les
vérifie, `crates/ritornello-core/src/core.rs:3276`).

`crates/ritornello-core/src/locales/en.toml` :

```toml
plugin_unreachable = "the plugin did not answer: it may have stopped running — see journalctl -u ritornello."
plugin_timeout = "the plugin took more than 5 s to answer: it is running but too slow, most often a network share that does not respond."
```

`deploy/locales/core/fr.toml` — **avec les accents**, c'est du texte affiché :

```toml
plugin_unreachable = "le plugin n'a pas répondu : il a peut-être cessé de tourner — voir journalctl -u ritornello."
plugin_timeout = "le plugin a mis plus de 5 s à répondre : il tourne, mais trop lentement — le plus souvent un partage réseau qui ne répond pas."
```

- [ ] **Étape 5 : tests au vert**

```
wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-files-navigateur && cargo test --workspace"
```
Attendu : tout vert, y compris le test de parité des catalogues du cœur et le
garde-fou qui refuse qu'une clé de catalogue atteigne l'écran. Rapporter les
chiffres.

- [ ] **Étape 6 : commit**

```bash
git add crates/ritornello-plugin-sdk/src crates/ritornello-core/src deploy/locales/core/fr.toml
git commit -m "fix(core): un plugin injoignable dit pourquoi, et le delai ne se confond plus avec la panne"
```

---

### Tâche 9 : un GET en échec lit la cause, comme un PUT

Ajoutée en cours de chantier, et c'est la seconde moitié de la tâche 8 —
mesurée dans le code, pas supposée : `send()` (donc `api.put` et `api.post`)
lit déjà `{"error": …}` du corps d'une réponse en échec, mais `api.get` lève
`` `HTTP ${r.status}` `` **sans regarder le corps**
(`web/kit/src/api.ts:34-36`). Or la page d'administration d'un plugin charge son
état par `api.get` : sans cette moitié, la cause portée par le 502 de la tâche 8
n'atteindrait l'écran que sur un PUT, et le chargement de page continuerait
d'afficher « Erreur : HTTP 502 ».

**Fichiers :**
- Modifier : `web/kit/src/api.ts`
- Test : `web/kit/src/api.test.ts`

**Interfaces :**
- Consomme : les corps `{"error": …}` que la tâche 8 fait porter aux 502.
- Produit : rien de nouveau à l'extérieur. `api.get` continue de **lever** une
  exception, `api.put`/`api.post` de **rendre** une chaîne — cette asymétrie est
  la convention établie du kit, et la changer casserait tous les appelants.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `web/kit/src/api.test.ts`, ajouter (le test existant « get rejette sur un
statut non ok » utilise un corps non-JSON et doit rester **inchangé** : il
couvre le repli) :

```ts
  it('get remonte la cause portée par le corps, pas le code nu', async () => {
    // Mesuré : le cœur fait porter sa cause au corps d'un 502, mais seul `send`
    // la lisait. Le chargement d'une page de plugin passe par `get`, et
    // affichait « HTTP 502 » là où le même échec sur un PUT disait pourquoi.
    mockFetch(
      new Response(JSON.stringify({ error: 'le plugin a mis plus de 5 s à répondre' }), {
        status: 502,
      }),
    )
    await expect(api.get('/x')).rejects.toThrow('plus de 5 s')
  })
```

- [ ] **Étape 2 : le lancer et vérifier qu'il échoue**

Depuis `web/kit` : `npx vitest run src/api.test.ts`
Attendu : ÉCHEC — l'exception porte `HTTP 502` au lieu de la phrase.

- [ ] **Étape 3 : implémenter**

Dans `web/kit/src/api.ts`, extraire la lecture du corps et l'employer des deux
côtés :

```ts
/**
 * Message d'une réponse en échec : le champ `error` du corps JSON quand il y en
 * a un, `HTTP <code>` sinon.
 *
 * Partagée par `send` et `get`, et c'est le correctif d'un défaut mesuré : seul
 * `send` lisait le corps, si bien qu'un 502 du cœur — qui porte pourtant sa
 * cause — s'affichait « HTTP 502 » au chargement d'une page, là où le même
 * échec sur un PUT disait ce qui n'allait pas.
 */
async function messageDErreur(r: Response): Promise<string> {
  try {
    const j = (await r.json()) as { error?: string }
    if (j && typeof j.error === 'string') return j.error
  } catch {
    // corps non JSON : on retombe sur le code
  }
  return `HTTP ${r.status}`
}
```

`send` perd son bloc `try`/`catch` de lecture du corps au profit d'un appel :

```ts
  if (r.ok) return null
  return messageDErreur(r)
```

et `get` cesse de rendre un code nu :

```ts
  async get<T>(url: string, init?: RequestInit): Promise<T> {
    const r = await fetch(url, init)
    if (!r.ok) throw new Error(await messageDErreur(r))
    return (await r.json()) as T
  },
```

- [ ] **Étape 4 : tests au vert**

Depuis `web/kit` : `npx vitest run`
Puis, parce que le kit est consommé par les deux autres ateliers :
depuis `web/app` : `npx vitest run`
et depuis `crates/ritornello-plugin-files/ui` : `npx vitest run`

Attendu : tout vert. Si un test d'un autre atelier affirmait `HTTP 502` sur un
GET, il faut le lire avant de conclure : c'est peut-être lui qu'il faut
amender — mais **seulement** s'il décrit un corps JSON portant `error`.
Rapporter les chiffres des trois suites.

- [ ] **Étape 5 : commit**

```bash
git add web/kit/src/api.ts web/kit/src/api.test.ts
git commit -m "fix(kit): un GET en echec remonte la cause du corps, pas le code nu"
```
