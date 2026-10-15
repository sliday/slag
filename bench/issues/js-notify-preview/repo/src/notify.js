"use strict";

const { makePreview } = require("./preview");

const PREVIEW_CHARS = 20;

/**
 * Build the notification payload for a user and a message body.
 */
function buildNotification(user, body) {
  return {
    title: `Message for ${user}`,
    preview: makePreview(body, PREVIEW_CHARS),
    body,
  };
}

module.exports = { buildNotification, PREVIEW_CHARS };
