import type { ReactNode } from "react";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "./ui/sheet";
import type { Contact } from "../types";
import { formatClassificationMethod, formatContactKind } from "../features/contactFormat";

const yesNo = (value: boolean) => (value ? "Sì" : "No");
const list = (values: string[] | null | undefined) => (values?.length ? values.join(" · ") : null);

/** Full record of one contact, grouped; the table only shows the working columns. */
function sections(c: Contact): DetailSection[] {
  return [
    [
      "Email",
      [
        ["Email originale", c.raw_email],
        ["Chiave confronto", c.email_comparison_key],
        ["Normalizzazione", c.normalization_method],
        ["PEC", yesNo(c.is_pec)],
        ["Casella di ruolo", yesNo(c.is_role_mailbox)],
        ["Usa e getta", yesNo(c.is_disposable)],
        ["Segnaposto", yesNo(c.is_placeholder)],
        ["Possibile refuso", yesNo(c.is_typo_suspected)],
      ],
    ],
    [
      "Ente e persona",
      [
        ["Organizzazione", c.organization_name],
        ["Denominazione", c.denomination],
        ["Persona", c.person_name],
        ["Ruolo", c.role],
        ["Dipartimento", c.department],
        ["Codice ufficiale", c.official_code],
        ["Tipo contatto", formatContactKind(c.contact_kind)],
        ["Sito web", c.website],
      ],
    ],
    [
      "Classificazione",
      [
        ["Categoria", c.category],
        ["Sottocategoria", c.subcategory],
        ["Metodo", formatClassificationMethod(c.classification_method)],
        ["Confidenza", `${Math.round(c.classification_confidence * 100)}%`],
        ["Stato", c.classification_state],
        ["Motivo stato", c.classification_state_reason],
        ["Evidenze", list(c.classification_evidence)],
        ["Da revisionare", yesNo(c.needs_review)],
        ["Versione tassonomia", c.taxonomy_version],
      ],
    ],
    [
      "Località",
      [
        ["Indirizzo", c.address],
        ["CAP", c.postal_code],
        ["Comune", c.comune],
        ["Provincia", c.provincia],
        ["Regione", c.regione],
        ["Macroarea", c.macroarea],
        ["Paese", c.paese],
      ],
    ],
    [
      "Verifica",
      [
        ["Punteggio", c.quality_score],
        ["Stato", c.status],
        ["Note validazione", list(c.validation_notes)],
        ["Instradamento", c.routing_status],
        ["Casella", c.mailbox_status],
        ["Ultima conferma", c.last_confirmed_at],
      ],
    ],
    [
      "Fonti",
      [
        ["URL origine", c.source_url],
        ["Occorrenze", c.source_occurrence_count],
        ["Fonti indipendenti", c.independent_source_count],
        ["Prima occorrenza", c.first_seen_at],
        ["Ultima occorrenza", c.last_seen_at],
        ["ID contatto", c.id],
      ],
    ],
  ];
}

export type DetailSection = [string, [string, ReactNode][]];

/** Right-hand sheet listing a record's fields by section; `—` for empty values. */
export function DetailsSheet({
  title,
  description,
  sections,
  onClose,
}: {
  title: string | null;
  description: string;
  sections: DetailSection[];
  onClose: () => void;
}) {
  return (
    <Sheet open={title !== null} onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="contact-details gap-0 overflow-y-auto pb-3 data-[side=right]:sm:max-w-md">
        {title !== null && (
          <>
            <SheetHeader className="p-5 pr-14">
              <SheetTitle className="truncate">{title}</SheetTitle>
              <SheetDescription>{description}</SheetDescription>
            </SheetHeader>
            {sections.map(([heading, rows]) => (
              <section key={heading} className="contact-details-section" aria-label={heading}>
                <h3>{heading}</h3>
                <dl>
                  {rows.map(([label, value]) => (
                    <div key={label}>
                      <dt>{label}</dt>
                      <dd>{value === null || value === undefined || value === "" ? "—" : value}</dd>
                    </div>
                  ))}
                </dl>
              </section>
            ))}
          </>
        )}
      </SheetContent>
    </Sheet>
  );
}

export function ContactDetailsSheet({
  contact,
  onClose,
}: {
  contact: Contact | null;
  onClose: () => void;
}) {
  return (
    <DetailsSheet
      title={contact?.email ?? null}
      description={
        contact ? [contact.category, contact.subcategory].filter(Boolean).join(" / ") : ""
      }
      sections={contact ? sections(contact) : []}
      onClose={onClose}
    />
  );
}
