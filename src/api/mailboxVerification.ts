import { invoke } from "@tauri-apps/api/core";
import type {
  MailboxVerificationAction,
  MailboxDnsRevalidationReport,
  MailboxVerificationMode,
  MailboxVerificationSettings,
} from "../types";

export const getMailboxVerificationOverview = () =>
  invoke<unknown>("get_mailbox_verification_overview");

export const saveMailboxVerificationSettings = (settings: MailboxVerificationSettings) =>
  invoke<unknown>("save_mailbox_verification_settings", {
    settings: {
      ehlo_hostname: settings.ehlo_hostname,
      envelope_sender: settings.envelope_sender,
      starttls_required: true,
      global_rate_per_minute: settings.global_rate_per_minute,
      max_concurrency: 1,
    },
  });

export const startMailboxVerification = (mode: MailboxVerificationMode) =>
  invoke<unknown>("start_mailbox_verification", { mode });

export const controlMailboxVerification = (action: MailboxVerificationAction) =>
  invoke<void>("control_mailbox_verification", { action });

export const getMailboxVerificationStatus = () =>
  invoke<unknown>("get_mailbox_verification_status", { jobId: null });

export const revalidateMailboxDns = () =>
  invoke<MailboxDnsRevalidationReport>("revalidate_mailbox_dns");
