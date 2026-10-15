"use strict";

/** Return display initials for a player name, e.g. "Ada Lovelace" -> "AL". */
function initials(name) {
  return name
    .split(/\s+/)
    .filter(Boolean)
    .map((part) => part[0].toUpperCase())
    .join("");
}

/** Format one leaderboard row for the CLI table. */
function formatRow(player) {
  const rank = String(player.rank ?? "-").padStart(3, " ");
  return `${rank}  ${initials(player.name)}  ${player.name} (${player.points} pts)`;
}

module.exports = { initials, formatRow };
