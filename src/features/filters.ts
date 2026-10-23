import type { FieldFilter, FilterOptions, Filters } from "../types";

const emptyField = (): FieldFilter => ({ include: [], exclude: [] });
export const emptyFilters = (): Filters => ({
  query: "",
  source_file: emptyField(),
  category: emptyField(),
  subcategory: emptyField(),
  contact_kind: emptyField(),
  paese: emptyField(),
  macroarea: emptyField(),
  regione: emptyField(),
  provincia: emptyField(),
  comune: emptyField(),
  pec: emptyField(),
  classification_method: emptyField(),
  classification_confidence: emptyField(),
  min_score: null,
  sort: null,
});
export const emptyFilterOptions = (): FilterOptions => ({
  source_file: [],
  category: [],
  subcategory: [],
  contact_kind: [],
  paese: [],
  macroarea: [],
  regione: [],
  provincia: [],
  comune: [],
  pec: [],
  classification_method: [],
  classification_confidence: [],
});
export const contactCategories = [
  "Istruzione",
  "Pubblica amministrazione",
  "Sanità",
  "Enti religiosi",
  "Associazioni",
  "Cultura",
  "Turismo e promozione territoriale",
  "Varie",
];
