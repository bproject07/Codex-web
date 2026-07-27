const ANDROID_USER_AGENT_PATTERN = /\bAndroid\b/i;
const ENTER_COMPANION_WINDOW_MS = 150;
const COMPOSITION_ENTER_WINDOW_MS = 1_000;
const FOCUS_REPORTS = new Set(["\u001b[I", "\u001b[O"]);

export type AndroidImePendingSource =
  | "key229"
  | "compositionend"
  | null;

export interface AndroidImeInputDecision {
  pendingSource: AndroidImePendingSource;
  inputType: string;
  textareaChanged: boolean;
}

interface PendingImeInput {
  source: Exclude<AndroidImePendingSource, null>;
  textareaValueBefore: string;
}

interface EnterTransaction {
  mode: "xterm" | "composition" | "soft";
  canonicalHandled: boolean;
  compositionEnded: boolean;
  enterEmitted: boolean;
  deferredKeydowns: DeferredKeydown[];
}

interface DeferredKeydown {
  init: KeyboardEventInit;
  keyCode: number;
  which: number;
  charCode: number;
}

interface PendingBlurRestore {
  value: string;
  restored: boolean;
}

interface AndroidImeGuardOptions {
  enabled: boolean;
  onTerminalInput: (data: string) => void;
  onDuplicateInputSuppressed?: () => void;
  onDuplicateEnterSuppressed?: () => void;
  onSoftEnterTranslated?: () => void;
}

export interface AndroidImeGuardDisposable {
  dispose: () => void;
  observeTerminalData: (data: string) => void;
}

export function shouldEnableAndroidImeGuard(userAgent: string): boolean {
  return ANDROID_USER_AGENT_PATTERN.test(userAgent);
}

export function shouldSuppressAndroidImeInput({
  pendingSource,
  inputType,
  textareaChanged,
}: AndroidImeInputDecision): boolean {
  if (inputType !== "insertText") {
    return false;
  }

  if (pendingSource === "compositionend") {
    return true;
  }

  return pendingSource === "key229" && textareaChanged;
}

export function isEnterKeyboardEvent(
  event: Pick<KeyboardEvent, "key" | "keyCode" | "which" | "charCode">,
): boolean {
  return (
    event.key === "Enter" ||
    event.keyCode === 13 ||
    event.which === 13 ||
    event.charCode === 13
  );
}

export function isEnterInputType(inputType: string): boolean {
  return inputType === "insertLineBreak" || inputType === "insertParagraph";
}

export function installAndroidImeGuard(
  container: HTMLElement,
  textarea: HTMLTextAreaElement,
  {
    enabled,
    onTerminalInput,
    onDuplicateInputSuppressed,
    onDuplicateEnterSuppressed,
    onSoftEnterTranslated,
  }: AndroidImeGuardOptions,
): AndroidImeGuardDisposable {
  if (!enabled) {
    return {
      dispose: () => undefined,
      observeTerminalData: () => undefined,
    };
  }

  let isComposing = false;
  let pendingInput: PendingImeInput | null = null;
  let pendingInputClearTimer: number | null = null;
  let enterTransaction: EnterTransaction | null = null;
  let enterTransactionTimer: number | null = null;
  let softEnterFallbackTimer: number | null = null;
  let pendingBlurRestore: PendingBlurRestore | null = null;
  let compositionCommitPending = false;
  let suppressNextCompositionEnd = false;
  const deferredEnterTimers = new Set<number>();

  const dispatchSyntheticCompositionEnd = () => {
    textarea.dispatchEvent(
      new CompositionEvent("compositionend", {
        bubbles: true,
        composed: true,
        data: "",
      }),
    );
    suppressNextCompositionEnd = true;
  };

  const clearPendingInput = () => {
    if (pendingInputClearTimer !== null) {
      window.clearTimeout(pendingInputClearTimer);
      pendingInputClearTimer = null;
    }
    pendingInput = null;
  };

  const armPendingInput = (source: PendingImeInput["source"]) => {
    clearPendingInput();
    const armedInput: PendingImeInput = {
      source,
      textareaValueBefore: textarea.value,
    };
    pendingInput = armedInput;
    // Native key/composition and input events belonging to one IME edit are
    // dispatched in the same browser task. Queue the expiry after target
    // listeners have scheduled xterm's helper callback. observeTerminalData
    // clears it earlier when that canonical helper callback emits.
    queueMicrotask(() => {
      if (pendingInput !== armedInput) {
        return;
      }
      pendingInputClearTimer = window.setTimeout(() => {
        if (pendingInput === armedInput) {
          clearPendingInput();
        }
      }, 0);
    });
  };

  const emitTranslatedEnter = () => {
    onSoftEnterTranslated?.();
    onTerminalInput("\r");
  };

  const retainEnterTransaction = (
    transaction: EnterTransaction,
    delay: number,
  ) => {
    if (enterTransactionTimer !== null) {
      window.clearTimeout(enterTransactionTimer);
    }
    enterTransactionTimer = window.setTimeout(() => {
      enterTransactionTimer = null;
      if (enterTransaction !== transaction) {
        return;
      }
      if (
        transaction.mode === "composition" &&
        !transaction.canonicalHandled
      ) {
        if (!transaction.compositionEnded) {
          // A few Android IMEs omit compositionend for their Send action.
          // Finish xterm's own composition helper so the pending text remains
          // canonical, then let the normal ordered submit path send Enter.
          dispatchSyntheticCompositionEnd();
          return;
        }
        transaction.canonicalHandled = true;
        transaction.enterEmitted = true;
        emitTranslatedEnter();
        replayDeferredKeydowns(transaction);
      }
      enterTransaction = null;
    }, delay);
  };

  const clearEnterTransaction = (flushPendingEnter = false) => {
    if (enterTransactionTimer !== null) {
      window.clearTimeout(enterTransactionTimer);
      enterTransactionTimer = null;
    }
    if (softEnterFallbackTimer !== null) {
      window.clearTimeout(softEnterFallbackTimer);
      softEnterFallbackTimer = null;
    }
    if (
      flushPendingEnter &&
      enterTransaction &&
      !enterTransaction.enterEmitted
    ) {
      enterTransaction.canonicalHandled = true;
      enterTransaction.enterEmitted = true;
      emitTranslatedEnter();
    }
    enterTransaction = null;
  };

  const emitEnterOnce = (transaction: EnterTransaction): boolean => {
    if (
      enterTransaction !== transaction ||
      transaction.canonicalHandled
    ) {
      return false;
    }
    transaction.canonicalHandled = true;
    transaction.enterEmitted = true;
    emitTranslatedEnter();
    retainEnterTransaction(transaction, ENTER_COMPANION_WINDOW_MS);
    return true;
  };

  const beginEnterTransaction = (
    mode: EnterTransaction["mode"],
  ): EnterTransaction => {
    clearEnterTransaction(true);
    const transaction: EnterTransaction = {
      mode,
      canonicalHandled: mode === "xterm",
      compositionEnded: false,
      enterEmitted: mode === "xterm",
      deferredKeydowns: [],
    };
    enterTransaction = transaction;
    retainEnterTransaction(
      transaction,
      mode === "composition"
        ? COMPOSITION_ENTER_WINDOW_MS
        : ENTER_COMPANION_WINDOW_MS,
    );
    if (mode === "soft") {
      softEnterFallbackTimer = window.setTimeout(() => {
        softEnterFallbackTimer = null;
        emitEnterOnce(transaction);
      }, 0);
    }
    return transaction;
  };

  const captureDeferredKeydown = (
    event: KeyboardEvent,
  ): DeferredKeydown => ({
    init: {
      bubbles: true,
      cancelable: true,
      composed: true,
      key: event.key,
      code: event.code,
      location: event.location,
      ctrlKey: event.ctrlKey,
      shiftKey: event.shiftKey,
      altKey: event.altKey,
      metaKey: event.metaKey,
      repeat: event.repeat,
      isComposing: false,
    },
    keyCode: event.keyCode,
    which: event.which,
    charCode: event.charCode,
  });

  const replayDeferredKeydowns = (transaction: EnterTransaction) => {
    if (transaction.deferredKeydowns.length === 0) {
      return;
    }
    const keydowns = transaction.deferredKeydowns.splice(0);
    if (enterTransaction === transaction) {
      clearEnterTransaction(false);
    }
    for (const keydown of keydowns) {
      const event = new KeyboardEvent("keydown", keydown.init);
      Object.defineProperties(event, {
        keyCode: { get: () => keydown.keyCode },
        which: { get: () => keydown.which },
        charCode: { get: () => keydown.charCode },
      });
      textarea.dispatchEvent(event);
    }
  };

  const scheduleCompositionSubmit = (transaction: EnterTransaction) => {
    if (
      enterTransaction !== transaction ||
      transaction.mode !== "composition" ||
      !transaction.compositionEnded ||
      transaction.canonicalHandled
    ) {
      return;
    }
    transaction.canonicalHandled = true;
    retainEnterTransaction(transaction, ENTER_COMPANION_WINDOW_MS);
    // compositionend schedules xterm's final text emission with setTimeout(0).
    // This timer is registered after that helper callback, preserving
    // text-before-CR ordering.
    const timer = window.setTimeout(() => {
      deferredEnterTimers.delete(timer);
      transaction.enterEmitted = true;
      emitTranslatedEnter();
      replayDeferredKeydowns(transaction);
    }, 0);
    deferredEnterTimers.add(timer);
  };

  const suppressEnterCompanion = (
    event: KeyboardEvent | InputEvent,
  ) => {
    event.preventDefault();
    event.stopPropagation();
    onDuplicateEnterSuppressed?.();
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.target !== textarea) {
      return;
    }

    const pendingCompositionTransaction = enterTransaction;
    if (
      pendingCompositionTransaction?.mode === "composition" &&
      !pendingCompositionTransaction.enterEmitted
    ) {
      event.preventDefault();
      event.stopPropagation();
      pendingCompositionTransaction.deferredKeydowns.push(
        captureDeferredKeydown(event),
      );
      if (!pendingCompositionTransaction.compositionEnded) {
        dispatchSyntheticCompositionEnd();
      } else {
        scheduleCompositionSubmit(pendingCompositionTransaction);
      }
      return;
    }

    const isEnter = isEnterKeyboardEvent(event);
    if (!isEnter) {
      clearEnterTransaction(true);
      if (event.keyCode === 229) {
        if (!isComposing) {
          armPendingInput("key229");
        }
      } else if (pendingInput?.source === "key229") {
        clearPendingInput();
      }
      return;
    }

    if (isComposing) {
      clearPendingInput();
      beginEnterTransaction("composition");
      // Let the browser finish the IME transaction, but keep xterm from
      // finalizing it before compositionupdate has settled.
      event.stopPropagation();
      return;
    }

    if (isEnter && event.keyCode === 229) {
      clearPendingInput();
      beginEnterTransaction("soft");
      // xterm treats 229 as an IME edit and otherwise drops this Enter.
      event.stopPropagation();
      return;
    }

    beginEnterTransaction("xterm");
  };

  const onKeyPress = (event: KeyboardEvent) => {
    if (event.target !== textarea || !isEnterKeyboardEvent(event)) {
      return;
    }

    const transaction = enterTransaction;
    if (!transaction) {
      return;
    }

    if (transaction.mode === "soft") {
      event.preventDefault();
      event.stopPropagation();
      if (!emitEnterOnce(transaction)) {
        onDuplicateEnterSuppressed?.();
      }
      return;
    }

    suppressEnterCompanion(event);
  };

  const onCompositionStart = (event: CompositionEvent) => {
    if (event.target !== textarea) {
      return;
    }
    suppressNextCompositionEnd = false;
    const pendingCompositionTransaction = enterTransaction;
    if (
      pendingCompositionTransaction?.mode === "composition" &&
      !pendingCompositionTransaction.enterEmitted
    ) {
      if (!pendingCompositionTransaction.compositionEnded) {
        dispatchSyntheticCompositionEnd();
      }
      scheduleCompositionSubmit(pendingCompositionTransaction);
      // The new composition now owns future compositionend events. The old
      // helper and CR timers retain their transaction directly and will still
      // emit text -> Enter before this new composition is committed.
      if (enterTransaction === pendingCompositionTransaction) {
        enterTransaction = null;
      }
      suppressNextCompositionEnd = false;
    }
    isComposing = true;
    clearPendingInput();
  };

  const onCompositionEnd = (event: CompositionEvent) => {
    if (event.target !== textarea) {
      return;
    }
    isComposing = false;
    if (suppressNextCompositionEnd) {
      suppressNextCompositionEnd = false;
      event.stopPropagation();
      armPendingInput("compositionend");
      return;
    }
    const transaction = enterTransaction;
    if (transaction?.mode === "composition") {
      if (transaction.compositionEnded) {
        // Ignore a late native compositionend after the bounded synthetic
        // fallback already finalized xterm's helper.
        event.stopPropagation();
        armPendingInput("compositionend");
        return;
      }
      transaction.compositionEnded = true;
      compositionCommitPending = true;
      queueMicrotask(() => {
        scheduleCompositionSubmit(transaction);
      });
    }
    armPendingInput("compositionend");
  };

  const onInput = (event: Event) => {
    if (event.target !== textarea || !(event instanceof InputEvent)) {
      return;
    }

    const currentPendingInput = pendingInput;

    if (isEnterInputType(event.inputType)) {
      if (currentPendingInput?.source !== "compositionend") {
        clearPendingInput();
      }
      event.stopPropagation();
      const transaction = enterTransaction;
      if (!transaction) {
        emitTranslatedEnter();
        return;
      }

      if (transaction.mode === "soft") {
        if (!emitEnterOnce(transaction)) {
          onDuplicateEnterSuppressed?.();
        }
        return;
      }

      if (transaction.mode === "composition") {
        if (transaction.canonicalHandled) {
          onDuplicateEnterSuppressed?.();
        } else if (transaction.compositionEnded) {
          scheduleCompositionSubmit(transaction);
        }
        return;
      }

      onDuplicateEnterSuppressed?.();
      return;
    }

    clearPendingInput();
    if (currentPendingInput) {
      const suppress = shouldSuppressAndroidImeInput({
        pendingSource: currentPendingInput.source,
        inputType: event.inputType,
        textareaChanged:
          textarea.value !== currentPendingInput.textareaValueBefore,
      });
      if (suppress) {
        // The textarea mutation has already happened. Stopping propagation
        // keeps xterm's direct input handler from sending it a second time;
        // its CompositionHelper transaction emits the canonical input.
        event.stopPropagation();
        onDuplicateInputSuppressed?.();
      }
    }

    const transaction = enterTransaction;
    if (
      transaction?.mode === "composition" &&
      transaction.compositionEnded
    ) {
      scheduleCompositionSubmit(transaction);
    }
  };

  const onBlur = () => {
    isComposing = false;
    clearPendingInput();
    if (enterTransaction?.mode !== "composition") {
      clearEnterTransaction(true);
    }
  };

  const onBlurCapture = (event: FocusEvent) => {
    if (event.target !== textarea) {
      return;
    }
    const transaction = enterTransaction;
    if (
      !compositionCommitPending &&
      (
        transaction?.mode !== "composition" ||
        transaction.compositionEnded
      )
    ) {
      return;
    }

    const restore: PendingBlurRestore = {
      value: textarea.value,
      restored: false,
    };
    pendingBlurRestore = restore;
    queueMicrotask(() => {
      if (pendingBlurRestore !== restore) {
        return;
      }
      if (textarea.value !== "" && textarea.value !== restore.value) {
        pendingBlurRestore = null;
        return;
      }
      // xterm clears its hidden textarea on blur. Restore it only until the
      // already-scheduled composition helper has emitted the pending commit.
      textarea.value = restore.value;
      restore.restored = true;
    });
  };

  const observeTerminalData = (data: string) => {
    if (
      (compositionCommitPending || pendingBlurRestore) &&
      FOCUS_REPORTS.has(data)
    ) {
      return;
    }
    clearPendingInput();
    compositionCommitPending = false;
    const restore = pendingBlurRestore;
    if (restore) {
      if (restore.restored && document.activeElement !== textarea) {
        textarea.value = "";
      }
      pendingBlurRestore = null;
    }
  };

  container.addEventListener("keydown", onKeyDown, true);
  container.addEventListener("keypress", onKeyPress, true);
  container.addEventListener("compositionstart", onCompositionStart, true);
  container.addEventListener("compositionend", onCompositionEnd, true);
  container.addEventListener("input", onInput, true);
  container.addEventListener("blur", onBlurCapture, true);
  textarea.addEventListener("blur", onBlur);

  return {
    observeTerminalData,
    dispose: () => {
      container.removeEventListener("keydown", onKeyDown, true);
      container.removeEventListener("keypress", onKeyPress, true);
      container.removeEventListener(
        "compositionstart",
        onCompositionStart,
        true,
      );
      container.removeEventListener("compositionend", onCompositionEnd, true);
      container.removeEventListener("input", onInput, true);
      container.removeEventListener("blur", onBlurCapture, true);
      textarea.removeEventListener("blur", onBlur);
      isComposing = false;
      clearPendingInput();
      clearEnterTransaction(false);
      pendingBlurRestore = null;
      compositionCommitPending = false;
      suppressNextCompositionEnd = false;
      for (const timer of deferredEnterTimers) {
        window.clearTimeout(timer);
      }
      deferredEnterTimers.clear();
    },
  };
}
