/**
 * Labels of `classification_method`. `CONTACT_SORT_COLUMNS` (contacts.rs) repeats
 * them to sort the classification column in the order shown: keep both in step.
 */
export function formatClassificationMethod(value: string): string {
  return (
    {
      ai: "IA",
      deterministic: "Deterministico",
      rule: "Regola",
      column: "Colonna",
      legacy: "Importazione",
      revalidated: "Rivalidazione DNS",
      taxonomy_default: "Sottocategoria generica",
    }[value.toLocaleLowerCase("en")] ?? value
  );
}

export function formatContactKind(value: string): string {
  return (
    {
      istituzionale: "Istituzionale",
      personale: "Personale",
      non_determinato: "Non determinato",
    }[value.toLocaleLowerCase("it")] ?? value
  );
}

/** Classification states whose labels are excluded from exports by default. */
const UNTRUSTED_STATES: Record<string, string> = {
  insufficient_evidence: "evidenze insufficienti",
  conflicting: "in conflitto",
  taxonomy_gap: "lacuna tassonomica",
};

export function isUntrustedClassification(state: string): boolean {
  return state in UNTRUSTED_STATES;
}

/**
 * One-cell summary of how a contact was classified, e.g. `IA`. The stored
 * confidence is a fixed value per method, not a measure of this contact, so the
 * cell does not show it.
 */
export function formatClassification(method: string, state: string): string {
  const base = formatClassificationMethod(method);
  const warning = UNTRUSTED_STATES[state];
  return warning ? `${base} · ${warning}` : base;
}
