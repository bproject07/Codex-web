import { describe, expect, it } from "vitest";
import {
  isEnterKeyboardEvent,
  isEnterInputType,
  shouldEnableAndroidImeGuard,
  shouldSuppressAndroidImeInput,
} from "./androidImeGuard";

describe("shouldEnableAndroidImeGuard", () => {
  it("enables the guard for Android, including hybrid pointer modes", () => {
    const android =
      "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 Chrome/150 Mobile";

    expect(shouldEnableAndroidImeGuard(android)).toBe(true);
    expect(
      shouldEnableAndroidImeGuard(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)",
      ),
    ).toBe(false);
  });
});

describe("shouldSuppressAndroidImeInput", () => {
  it("suppresses the direct non-composed input paired with keyCode 229", () => {
    expect(
      shouldSuppressAndroidImeInput({
        pendingSource: "key229",
        inputType: "insertText",
        textareaChanged: true,
      }),
    ).toBe(true);
  });

  it("lets xterm handle keyCode 229 input when no textarea diff is pending", () => {
    expect(
      shouldSuppressAndroidImeInput({
        pendingSource: "key229",
        inputType: "insertText",
        textareaChanged: false,
      }),
    ).toBe(false);
  });

  it("suppresses keyCode 229 input even if keyup made it composed", () => {
    expect(
      shouldSuppressAndroidImeInput({
        pendingSource: "key229",
        inputType: "insertText",
        textareaChanged: true,
      }),
    ).toBe(true);
  });

  it("suppresses insertText paired with compositionend", () => {
    expect(
      shouldSuppressAndroidImeInput({
        pendingSource: "compositionend",
        inputType: "insertText",
        textareaChanged: false,
      }),
    ).toBe(true);
  });

  it("does not suppress unrelated input types or standalone input", () => {
    expect(
      shouldSuppressAndroidImeInput({
        pendingSource: "compositionend",
        inputType: "deleteContentBackward",
        textareaChanged: true,
      }),
    ).toBe(false);
    expect(
      shouldSuppressAndroidImeInput({
        pendingSource: null,
        inputType: "insertText",
        textareaChanged: true,
      }),
    ).toBe(false);
  });

});

describe("isEnterKeyboardEvent", () => {
  it("recognizes modern and legacy Android Enter events", () => {
    expect(
      isEnterKeyboardEvent({
        key: "Enter",
        keyCode: 0,
        which: 0,
        charCode: 0,
      }),
    ).toBe(true);
    expect(
      isEnterKeyboardEvent({
        key: "",
        keyCode: 0,
        which: 13,
        charCode: 13,
      }),
    ).toBe(true);
    expect(
      isEnterKeyboardEvent({
        key: "a",
        keyCode: 65,
        which: 65,
        charCode: 97,
      }),
    ).toBe(false);
  });
});

describe("isEnterInputType", () => {
  it("recognizes Android soft-keyboard line break input", () => {
    expect(isEnterInputType("insertLineBreak")).toBe(true);
    expect(isEnterInputType("insertParagraph")).toBe(true);
    expect(isEnterInputType("insertText")).toBe(false);
  });
});
