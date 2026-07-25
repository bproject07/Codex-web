import { describe, expect, it } from "vitest";
import {
  REPLAY_WRITE_BATCH_BYTES,
  takeReplayBatch,
  type BufferedReplay,
} from "./replay";

describe("replay batching", () => {
  it("preserves byte order while limiting every parser batch", () => {
    const expected = new Uint8Array(REPLAY_WRITE_BATCH_BYTES * 2 + 37);
    for (let index = 0; index < expected.length; index += 1) {
      expected[index] = index % 251;
    }
    const replay: BufferedReplay = {
      chunks: [
        expected.subarray(0, 7),
        expected.subarray(7, REPLAY_WRITE_BATCH_BYTES + 19),
        expected.subarray(REPLAY_WRITE_BATCH_BYTES + 19),
      ],
      byteLength: expected.byteLength,
    };

    const batches: Uint8Array[] = [];
    while (replay.byteLength > 0) {
      batches.push(takeReplayBatch(replay));
    }

    expect(batches).toHaveLength(3);
    expect(
      batches.every(
        (batch) =>
          batch.byteLength > 0 &&
          batch.byteLength <= REPLAY_WRITE_BATCH_BYTES,
      ),
    ).toBe(true);
    expect(Uint8Array.from(batches.flatMap((batch) => [...batch]))).toEqual(
      expected,
    );
    expect(replay.chunks).toHaveLength(0);
  });

  it("returns an empty batch for an empty replay", () => {
    const replay: BufferedReplay = { chunks: [], byteLength: 0 };

    expect(takeReplayBatch(replay)).toHaveLength(0);
  });
});
