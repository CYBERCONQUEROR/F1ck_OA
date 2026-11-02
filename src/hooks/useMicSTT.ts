import { useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

export type MicState = "idle" | "recording" | "transcribing" | "error";

export interface UseMicSTTReturn {
  micState: MicState;
  startMic: (durationSecs?: number) => Promise<string | null>;
  stopMic: () => void;
  error: string | null;
}

/**
 * Hook that records from the microphone via the Tauri mic_record_and_transcribe
 * command (which uses the bundled moonshine-voice sidecar) and returns the
 * transcript text so the caller can put it into the input field.
 */
export function useMicSTT(): UseMicSTTReturn {
  const [micState, setMicState] = useState<MicState>("idle");
  const [error, setError] = useState<string | null>(null);
  const abortRef = useRef(false);

  const stopMic = useCallback(() => {
    abortRef.current = true;
  }, []);

  const startMic = useCallback(
    async (durationSecs = 10): Promise<string | null> => {
      abortRef.current = false;
      setError(null);
      setMicState("recording");

      try {
        // Tauri command records mic for durationSecs then transcribes
        const transcript = await invoke<string>("mic_record_and_transcribe", {
          durationSecs,
        });

        if (abortRef.current) return null;

        setMicState("idle");
        return transcript?.trim() || null;
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setError(msg);
        setMicState("error");
        return null;
      }
    },
    []
  );

  return { micState, startMic, stopMic, error };
}
