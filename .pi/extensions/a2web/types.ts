export interface WelcomeMessage { type: "welcome"; }

export type AnnouncementKind =
  | "intent_registered"
  | "intent_unregistered"
  | "entity_registered"
  | "entity_unregistered";

export interface AnnouncementMessage {
  type: "announcement";
  kind: AnnouncementKind;
  app_id: string;
  intent_type: string;
  entity_id: string | null;
}

export interface EntityResponseMessage {
  type: "entity_response";
  op_id: string;
  kind: string;
  app_id: string;
  intent_type: string;
  entity_id: string;
  data: string | null;
  is_error: boolean;
  error: string | null;
}

export type HarnessMessage = WelcomeMessage | AnnouncementMessage | EntityResponseMessage;

export interface EntityOp {
  kind: "list" | "read" | "set" | "transfer";
  app_id: string;
  intent_type: string;
  entity_id: string;
  data?: string | null;
  target_app?: string | null;
}
