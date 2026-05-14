"use client";

import { useState, useCallback, useRef } from "react";

type RecordingState = "idle" | "recording" | "processing";

interface TranscribeResponse {
  text?: string;
  error?: string;
  details?: string;
  placeholder?: boolean;
}

function getSupportedMimeType(): string {
  const mimeTypes = ["audio/webm", "audio/mp4", "audio/ogg", "audio/wav"];
  for (const mimeType of mimeTypes) {
    if (MediaRecorder.isTypeSupported(mimeType)) {
      return mimeType;
    }
  }
  // Fallback - let the browser choose
  return "";
}

export function useAudioRecording() {
  const [state, setState] = useState<RecordingState>("idle");
  const [error, setError] = useState<string | null>(null);

  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const audioChunksRef = useRef<Blob[]>([]);
  const streamRef = useRef<MediaStream | null>(null);
  const mimeTypeRef = useRef<string>("");

  const startRecording = useCallback(async () => {
    setError(null);

    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      streamRef.current = stream;

      const mimeType = getSupportedMimeType();
      mimeTypeRef.current = mimeType;

      const mediaRecorder = new MediaRecorder(
        stream,
        mimeType ? { mimeType } : undefined,
      );
      mediaRecorderRef.current = mediaRecorder;
      audioChunksRef.current = [];

      mediaRecorder.ondataavailable = (event) => {
        if (event.data.size > 0) {
          audioChunksRef.current.push(event.data);
        }
      };

      mediaRecorder.start();
      setState("recording");
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (
        message.includes("Permission denied") ||
        message.includes("NotAllowedError")
      ) {
        setError(
          "Microphone access denied. Please allow microphone access to use voice input.",
        );
      } else {
        setError(`Failed to start recording: ${message}`);
      }
      setState("idle");
    }
  }, []);

  const stopRecording = useCallback(async (): Promise<string | null> => {
    const mediaRecorder = mediaRecorderRef.current;
    if (!mediaRecorder || state !== "recording") {
      return null;
    }

    return new Promise((resolve) => {
      mediaRecorder.onstop = async () => {
        // Stop all tracks to release the microphone
        streamRef.current?.getTracks().forEach((track) => track.stop());
        streamRef.current = null;

        setState("processing");

        const mimeType = mimeTypeRef.current || "audio/webm";
        const audioBlob = new Blob(audioChunksRef.current, {
          type: mimeType,
        });

        try {
          // POST the raw blob to cos-agent-bridge; Content-Type
          // carries the mime so the bridge can hand it to the STT
          // backend unchanged. No base64 = no 33% size blow-up.
          const response = await fetch("/api/voice/upload", {
            method: "POST",
            headers: { "Content-Type": mimeType },
            body: audioBlob,
          });

          const data = (await response.json()) as TranscribeResponse;

          if (!response.ok) {
            setError(data.error ?? "Transcription failed");
            setState("idle");
            resolve(null);
            return;
          }

          setState("idle");
          resolve(data.text ?? null);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          setError(`Transcription failed: ${message}`);
          setState("idle");
          resolve(null);
        }
      };

      mediaRecorder.stop();
    });
  }, [state]);

  const toggleRecording = useCallback(async (): Promise<string | null> => {
    if (state === "recording") {
      return stopRecording();
    } else if (state === "idle") {
      await startRecording();
      return null;
    }
    return null;
  }, [state, startRecording, stopRecording]);

  const clearError = useCallback(() => {
    setError(null);
  }, []);

  return {
    state,
    error,
    clearError,
    startRecording,
    stopRecording,
    toggleRecording,
    isRecording: state === "recording",
    isProcessing: state === "processing",
  };
}
