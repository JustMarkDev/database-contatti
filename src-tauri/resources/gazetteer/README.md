# Administrative gazetteer snapshot

`geonames-2026-07-25.tsv` is the repository-owned catalog used by the native
backend. It is embedded in the Rust binary and bundled as a native resource; it
is not a Vite/public frontend asset.

- **Version:** `geonames-2026-07-25` (retrieved 2026-07-25)
- **Source:** GeoNames country dumps `IT.zip` and `CH.zip`, plus
  `hierarchy.zip` for administrative parent links,
  <https://download.geonames.org/export/dump/>
- **Source version rule:** the dated repository snapshot, using current `ADM1`,
  `ADM2`, and `ADM3` rows from those two country files on the retrieval date.
- **License:** Creative Commons Attribution 4.0,
  <https://creativecommons.org/licenses/by/4.0/>. GeoNames attribution and data
  documentation: <https://www.geonames.org/export/>.

The transformation retains GeoNames IDs, canonical and alternate names. Italy
uses `ADM1` Regione → `ADM2` Provincia → `ADM3` Comune; Switzerland intentionally
uses `ADM1` Cantone → `ADM3` Comune (districts are not application fields).
Alternate names are aliases, and duplicate normalized names remain separate
rows so resolution reports ambiguity rather than selecting arbitrarily.
