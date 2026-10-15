"use strict";

/** In-memory user store, oldest signup first. */
function createStore() {
  const users = [];
  let nextId = 1;
  return {
    add(name, email) {
      const user = { id: nextId++, name, email };
      users.push(user);
      return user;
    },
    find(id) {
      return users.find((u) => u.id === id) || null;
    },
    all() {
      return users.slice();
    },
    count() {
      return users.length;
    },
  };
}

module.exports = { createStore };
