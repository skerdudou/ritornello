/// Version du contrat que le cœur expose aux modules d'IHM des plugins
/// (`vue` + `@ritornello/ui`). Un module de plugin exporte son propre
/// `contract` ; le shell refuse de le monter en cas d'écart, avec un message
/// explicite. À incrémenter à toute modification incompatible du kit.
export const UI_CONTRACT = 1
