import type { KeyboardEvent } from "react";

const TAB_NAVIGATION_KEYS = new Set(["ArrowLeft", "ArrowRight", "Home", "End"]);

export function activateTabFromKeyboard<T>(
  event: KeyboardEvent<HTMLElement>,
  tabs: readonly T[],
  currentIndex: number,
  activate: (tab: T) => void,
) {
  if (!TAB_NAVIGATION_KEYS.has(event.key) || tabs.length === 0) return;
  event.preventDefault();

  let nextIndex = currentIndex;
  if (event.key === "Home") nextIndex = 0;
  else if (event.key === "End") nextIndex = tabs.length - 1;
  else if (event.key === "ArrowLeft") nextIndex = (currentIndex - 1 + tabs.length) % tabs.length;
  else if (event.key === "ArrowRight") nextIndex = (currentIndex + 1) % tabs.length;

  activate(tabs[nextIndex]);
  const tabElements = event.currentTarget
    .closest('[role="tablist"]')
    ?.querySelectorAll<HTMLElement>('[role="tab"]');
  tabElements?.[nextIndex]?.focus();
}
