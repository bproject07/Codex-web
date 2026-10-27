import { expect, it } from "vitest";
import { parseGuestServerControl } from "./protocol";

it("accepts bounded guest metadata and replay controls", () => {
  const metadata = { type:"shared_session", sessionId:"generation", agent:"Codex", status:"running", cols:120, rows:35, archiveIncomplete:false };
  expect(parseGuestServerControl(JSON.stringify(metadata))).toEqual(metadata);
  expect(parseGuestServerControl('{"type":"replay_end","lastSequence":42}')).toEqual({type:"replay_end",lastSequence:42});
  expect(parseGuestServerControl('{"type":"flow_control","windowBytes":4194304}')).not.toBeNull();
});
it("rejects malformed, oversized and owner protocol messages", () => {
  for (const value of ["null", "[]", "{}", "{", '{"type":"session","session":{}}',
    '{"type":"replay_end","lastSequence":-1}', '{"type":"flow_control","windowBytes":4194305}',
    '{"type":"shared_session","cols":5000,"rows":35}', " ".repeat(4097)]) {
    expect(parseGuestServerControl(value)).toBeNull();
  }
});
