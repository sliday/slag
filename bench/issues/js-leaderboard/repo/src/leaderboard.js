"use strict";

/**
 * Return the top `limit` players by points, best first, with ranks
 * starting at 1.
 */
function topPlayers(players, limit = 10) {
  const sorted = players.slice().sort().reverse();
  return sorted.slice(0, limit).map((player, index) => ({
    rank: index + 1,
    ...player,
  }));
}

module.exports = { topPlayers };
