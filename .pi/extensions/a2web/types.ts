export interface WelcomeMessage { type: "welcome"; }
export interface ObservationMessage { type: "observation"; app_id: string; data: string; label: string | null; }
export type HarnessMessage = WelcomeMessage | ObservationMessage;

export interface ObservationInfo { app_id: string; data: string; label: string | null; sequence: number; }
