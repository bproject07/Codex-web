"use strict";

const ESC = "\u001b[";
let redrawTimer = null;

function color(red, green, blue, text) {
  return `${ESC}38;2;${red};${green};${blue}m${text}${ESC}0m`;
}

function boxedLine(width, text) {
  const contentWidth = width - 2;
  return (
    color(83, 198, 255, "│") +
    color(231, 245, 255, text.padEnd(contentWidth).slice(0, contentWidth)) +
    color(83, 198, 255, "│")
  );
}

function draw() {
  const width = Math.max(34, Math.min(process.stdout.columns ?? 100, 104));
  const compact = width < 64;
  const rule = "─".repeat(width - 2);
  const title = compact
    ? "  Community Terminal Demo"
    : "  Community Demo — Web PTY Bridge";
  const description = compact
    ? "  Synthetic demo — no account data."
    : "  Deterministic demo PTY — synthetic content and no account data.";
  const request = compact
    ? "  › Explore the terminal workflow"
    : "  › Explore a cross-platform terminal workflow";
  const note = compact
    ? "  • Illustrative screenshot output"
    : "  • Illustrative output prepared only for repository screenshots";
  const validation = compact
    ? "  • No model request or validation"
    : "  • No model request or project validation was performed";
  const lines = [
    `${ESC}3J${ESC}2J${ESC}H`,
    color(83, 198, 255, `┌${rule}┐`),
    boxedLine(width, title),
    color(83, 198, 255, `└${rule}┘`),
    "",
    color(139, 148, 158, description),
    "",
    `${color(63, 185, 80, "  ✓")} Native PTY connected`,
    `${color(63, 185, 80, "  ✓")} Authenticated WebSocket active`,
    `${color(63, 185, 80, "  ✓")} Replay and mobile controls ready`,
    "",
    color(210, 168, 255, request),
    "",
    color(139, 148, 158, note),
    color(139, 148, 158, validation),
    "",
    color(63, 185, 80, "  ✓ Demo ready — type to explore"),
    "",
    `${color(83, 198, 255, "  open-source-demo")}  ${color(139, 148, 158, "main")}`,
    color(231, 245, 255, "  › Type your next instruction… "),
  ];

  process.stdout.write(lines.join("\r\n"));
}

function scheduleDraw() {
  if (redrawTimer !== null) {
    clearTimeout(redrawTimer);
  }
  redrawTimer = setTimeout(() => {
    redrawTimer = null;
    draw();
  }, 60);
}

process.stdout.on("resize", scheduleDraw);
process.stdin.setRawMode?.(true);
process.stdin.resume();
process.stdin.on("data", (data) => {
  if (data.includes(3)) {
    process.exit(0);
  }
});

draw();
