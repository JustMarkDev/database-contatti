import { invoke } from "@tauri-apps/api/core";
import type {
  AiFieldChange,
  AiReport,
  AiRun,
  AiStartRequest,
  OpenCodeStatus,
  ReviewList,
  TypeSafeStatus,
  TypeSafeKeyView,
} from "../types";

export const detectOpencode = () => invoke<OpenCodeStatus>("detect_opencode");
export const detectTypesafe = () => invoke<TypeSafeStatus>("detect_typesafe");
export const getTypesafeApiKey = () => invoke<TypeSafeKeyView>("get_typesafe_api_key");
export const setTypesafeApiKey = (apiKey: string) =>
  invoke<TypeSafeKeyView>("set_typesafe_api_key", { apiKey });
export const listAiRuns = () => invoke<AiRun[]>("list_ai_runs", { limit: 20 });
export const getResumableAiRun = () => invoke<AiRun | null>("get_resumable_ai_run");
export const listAiFieldChanges = (runId: string, offset: number, limit: number) =>
  invoke<{ changes: AiFieldChange[]; total: number }>("list_ai_field_changes", {
    runId,
    limit,
    offset,
  });
export const startAiClassification = (request: AiStartRequest) =>
  invoke<AiReport>("start_ai_classification", { request });
export const pauseAiClassification = (runId: string) =>
  invoke("pause_ai_classification", { runId });
export const resumeAiClassification = (runId: string) =>
  invoke("resume_ai_classification", { runId });
export const cancelAiClassification = (runId: string) =>
  invoke("cancel_ai_classification", { runId });
export const listPendingReviews = () =>
  invoke<ReviewList>("list_classification_reviews", { status: "pending" });
export const approveReview = (reviewId: string) =>
  invoke("approve_classification_review", {
    reviewId,
    category: null,
    subcategory: null,
  });
export const rejectReview = (reviewId: string) =>
  invoke("reject_classification_review", { reviewId });
