export interface WelcomeMessage { type: "welcome"; }

export interface IntentRegisteredMessage {
  type: "intent_registered";
  app_id: string;
  intent_type: string;
  intent_id: string;
}

export interface IntentUnregisteredMessage {
  type: "intent_unregistered";
  app_id: string;
  intent_type: string;
  intent_id: string;
}

export interface IntentResponseMessage {
  type: "intent_response";
  op_id: string;
  kind: string;
  app_id: string;
  intent_id: string;
  data: string | null;
  is_error: boolean;
  error: string | null;
}

export type HarnessMessage =
  | WelcomeMessage
  | IntentRegisteredMessage
  | IntentUnregisteredMessage
  | IntentResponseMessage;

export interface IntentOp {
  kind: "get" | "set" | "transfer";
  app_id: string;
  intent_id: string;
  data?: string | null;
  target_app?: string | null;
}
