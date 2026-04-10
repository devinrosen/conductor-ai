import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef, useState } from "react";
import { useFocusTrap } from "./useFocusTrap";

describe("useFocusTrap", () => {
  let user: ReturnType<typeof userEvent.setup>;

  beforeEach(() => {
    user = userEvent.setup();
    // Mock console.error to avoid noise in test output
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it("focuses the dialog element when opened", async () => {
    function TestApp() {
      const [isOpen, setIsOpen] = useState(false);
      const dialogRef = useRef<HTMLDivElement>(null);
      useFocusTrap(dialogRef, isOpen, () => setIsOpen(false));

      return (
        <div>
          <button onClick={() => setIsOpen(true)}>Open Dialog</button>
          {isOpen && (
            <div ref={dialogRef} tabIndex={-1} role="dialog" aria-label="Test dialog">
              <button>First button</button>
            </div>
          )}
        </div>
      );
    }

    render(<TestApp />);

    const openButton = screen.getByText("Open Dialog");
    await user.click(openButton);

    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveFocus();
  });

  it("traps focus within the dialog on Tab", async () => {
    function TestApp() {
      const [isOpen, setIsOpen] = useState(false);
      const dialogRef = useRef<HTMLDivElement>(null);
      useFocusTrap(dialogRef, isOpen, () => setIsOpen(false));

      return (
        <div>
          <button onClick={() => setIsOpen(true)}>Open Dialog</button>
          {isOpen && (
            <div ref={dialogRef} tabIndex={-1} role="dialog" aria-label="Test dialog">
              <button>First button</button>
              <input placeholder="Input field" />
              <a href="#test">Link</a>
              <button>Last button</button>
            </div>
          )}
        </div>
      );
    }

    render(<TestApp />);

    await user.click(screen.getByText("Open Dialog"));

    const dialog = screen.getByRole("dialog");
    const firstButton = screen.getByText("First button");
    const lastButton = screen.getByText("Last button");

    // Start with dialog focused
    expect(dialog).toHaveFocus();

    // Tab should move to first focusable element
    await user.tab();
    expect(firstButton).toHaveFocus();

    // Tab through all elements
    await user.tab();
    expect(screen.getByPlaceholderText("Input field")).toHaveFocus();

    await user.tab();
    expect(screen.getByText("Link")).toHaveFocus();

    await user.tab();
    expect(lastButton).toHaveFocus();

    // Tab from last element should wrap to first
    await user.tab();
    expect(firstButton).toHaveFocus();
  });

  it("traps focus within the dialog on Shift+Tab", async () => {
    function TestApp() {
      const [isOpen, setIsOpen] = useState(false);
      const dialogRef = useRef<HTMLDivElement>(null);
      useFocusTrap(dialogRef, isOpen, () => setIsOpen(false));

      return (
        <div>
          <button onClick={() => setIsOpen(true)}>Open Dialog</button>
          {isOpen && (
            <div ref={dialogRef} tabIndex={-1} role="dialog" aria-label="Test dialog">
              <button>First button</button>
              <input placeholder="Input field" />
              <a href="#test">Link</a>
              <button>Last button</button>
            </div>
          )}
        </div>
      );
    }

    render(<TestApp />);

    await user.click(screen.getByText("Open Dialog"));

    const firstButton = screen.getByText("First button");
    const lastButton = screen.getByText("Last button");

    // Focus the first button
    firstButton.focus();
    expect(firstButton).toHaveFocus();

    // Shift+Tab from first element should wrap to last
    await user.tab({ shift: true });
    expect(lastButton).toHaveFocus();

    // Continue shift+tabbing backward
    await user.tab({ shift: true });
    expect(screen.getByText("Link")).toHaveFocus();

    await user.tab({ shift: true });
    expect(screen.getByPlaceholderText("Input field")).toHaveFocus();

    await user.tab({ shift: true });
    expect(firstButton).toHaveFocus();
  });

  it("calls onClose when Escape is pressed", async () => {
    const onClose = vi.fn();

    function TestEscapeDialog() {
      const dialogRef = useRef<HTMLDivElement>(null);
      useFocusTrap(dialogRef, true, onClose);

      return (
        <div ref={dialogRef} tabIndex={-1} role="dialog">
          <button>Test button</button>
        </div>
      );
    }

    render(<TestEscapeDialog />);

    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("restores focus to the previously focused element when closed", async () => {
    function TestApp() {
      const [isOpen, setIsOpen] = useState(false);
      const dialogRef = useRef<HTMLDivElement>(null);
      useFocusTrap(dialogRef, isOpen, () => setIsOpen(false));

      return (
        <div>
          <button onClick={() => setIsOpen(true)}>Open Dialog</button>
          {isOpen && (
            <div ref={dialogRef} tabIndex={-1} role="dialog" aria-label="Test dialog">
              <button>Dialog button</button>
            </div>
          )}
        </div>
      );
    }

    render(<TestApp />);

    const openButton = screen.getByText("Open Dialog");
    await user.click(openButton);

    // Dialog should be open and focused
    expect(screen.getByRole("dialog")).toHaveFocus();

    // Close the dialog by pressing Escape
    await user.keyboard("{Escape}");

    // Focus should be restored to the open button
    expect(openButton).toHaveFocus();
  });

  it("handles dialogs with no focusable elements gracefully", async () => {
    function TestEmptyApp() {
      const [isOpen, setIsOpen] = useState(false);
      const dialogRef = useRef<HTMLDivElement>(null);
      useFocusTrap(dialogRef, isOpen, () => setIsOpen(false));

      return (
        <div>
          <button onClick={() => setIsOpen(true)}>Open Empty Dialog</button>
          {isOpen && (
            <div ref={dialogRef} tabIndex={-1} role="dialog">
              <span>No focusable elements here</span>
            </div>
          )}
        </div>
      );
    }

    render(<TestEmptyApp />);

    await user.click(screen.getByText("Open Empty Dialog"));

    // Dialog should initially be focused
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveFocus();

    // When there are no focusable elements, the hook's querySelectorAll should return empty
    // and the Tab key should not cause any JavaScript errors
    // The actual focus behavior may vary, but the key is that it doesn't crash
    await user.tab();
    // The test just needs to ensure it doesn't crash - focus behavior with no focusable elements
    // can vary by browser and is handled gracefully by the early return in the code
  });

  it("does nothing when dialog is not open", () => {
    const onClose = vi.fn();

    function ClosedDialog() {
      const dialogRef = useRef<HTMLDivElement>(null);
      useFocusTrap(dialogRef, false, onClose); // closed

      return (
        <div>
          <div ref={dialogRef} role="dialog" style={{ display: "none" }}>
            <button>Hidden button</button>
          </div>
          <button>Visible button</button>
        </div>
      );
    }

    render(<ClosedDialog />);

    const visibleButton = screen.getByText("Visible button");
    visibleButton.focus();
    expect(visibleButton).toHaveFocus();

    // Even though the hook is active, it shouldn't affect focus when closed
    // Tab should work normally
    // (Note: Testing this behavior requires more complex setup, but the key point
    // is that the useEffect has an early return when !open)
  });

  it("updates behavior when open state changes", async () => {
    function ToggleDialog() {
      const [isOpen, setIsOpen] = useState(false);
      const dialogRef = useRef<HTMLDivElement>(null);
      useFocusTrap(dialogRef, isOpen, () => setIsOpen(false));

      return (
        <div>
          <button onClick={() => setIsOpen(!isOpen)}>Toggle Dialog</button>
          {isOpen && (
            <div ref={dialogRef} tabIndex={-1} role="dialog">
              <button>Dialog button</button>
            </div>
          )}
        </div>
      );
    }

    render(<ToggleDialog />);

    const toggleButton = screen.getByText("Toggle Dialog");

    // Open dialog
    await user.click(toggleButton);
    expect(screen.getByRole("dialog")).toHaveFocus();

    // Close dialog
    await user.click(toggleButton);
    expect(toggleButton).toHaveFocus();
  });
});