export interface PendingFileOpen {
  appId: 'texteditor';
  fileId: string;
  fileName: string;
}

let pendingFileOpen: PendingFileOpen | null = null;

export function queueFileOpen(request: PendingFileOpen) {
  pendingFileOpen = request;
}

export function takeFileOpen(appId: PendingFileOpen['appId']) {
  if (pendingFileOpen?.appId !== appId) return null;
  const request = pendingFileOpen;
  pendingFileOpen = null;
  return request;
}
