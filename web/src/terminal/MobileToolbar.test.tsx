import { Children, isValidElement, type ReactElement, type ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import { MobileToolbar } from "./MobileToolbar";
import { MOBILE_KEY_SEQUENCES } from "./mobileKeys";

vi.mock("../scrollFade", () => ({
  useHorizontalScrollFade: () => ({ current: null }),
}));

interface ButtonProps {
  children?: ReactNode;
  onClick?: () => void;
}

interface ElementWithChildren {
  children?: ReactNode;
}

function buttons(
  element: ReactElement<ElementWithChildren>,
): ReactElement<ButtonProps>[] {
  const inner = Children.toArray(element.props.children).find(
    (child): child is ReactElement<ElementWithChildren> =>
      isValidElement<ElementWithChildren>(child),
  );
  if (!inner) {
    throw new Error("mobile toolbar inner element is missing");
  }
  return Children.toArray(inner.props.children).filter(
    (child): child is ReactElement<ButtonProps> =>
      isValidElement<ButtonProps>(child) && child.type === "button",
  );
}

describe("MobileToolbar", () => {
  it("sends Page Up and Page Down as terminal input", () => {
    const onSend = vi.fn();
    const toolbar = MobileToolbar({
      ctrlMode: false,
      onCtrlModeChange: vi.fn(),
      onSend,
      onScrollToTop: vi.fn(),
      onScrollToBottom: vi.fn(),
      onHide: vi.fn(),
    });
    const renderedButtons = buttons(
      toolbar as ReactElement<ElementWithChildren>,
    );

    renderedButtons.find((button) => button.props.children === "PgUp")?.props.onClick?.();
    renderedButtons.find((button) => button.props.children === "PgDn")?.props.onClick?.();

    expect(onSend).toHaveBeenNthCalledWith(1, MOBILE_KEY_SEQUENCES.pageUp);
    expect(onSend).toHaveBeenNthCalledWith(2, MOBILE_KEY_SEQUENCES.pageDown);
  });
});
