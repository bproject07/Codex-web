export const REPLAY_WRITE_BATCH_BYTES = 16 * 1024;

export interface BufferedReplay {
  chunks: Uint8Array[];
  byteLength: number;
}

export function takeReplayBatch(
  replay: BufferedReplay,
  maximumBytes = REPLAY_WRITE_BATCH_BYTES,
): Uint8Array {
  if (maximumBytes < 1 || replay.byteLength < 1) {
    return new Uint8Array();
  }

  const length = Math.min(maximumBytes, replay.byteLength);
  const output = new Uint8Array(length);
  let offset = 0;

  while (offset < length) {
    const chunk = replay.chunks[0];
    if (!chunk) {
      replay.byteLength = 0;
      return output.subarray(0, offset);
    }

    const bytesToTake = Math.min(chunk.byteLength, length - offset);
    output.set(chunk.subarray(0, bytesToTake), offset);
    offset += bytesToTake;

    if (bytesToTake === chunk.byteLength) {
      replay.chunks.shift();
    } else {
      replay.chunks[0] = chunk.subarray(bytesToTake);
    }
  }

  replay.byteLength -= length;
  return output;
}
