# Push previews break on emoji

Users with emoji in their messages get mangled previews. Two symptoms:

1. Some previews end in a hollow rectangle or a replacement character
   (shown as an empty box on Android). This happens when the cut lands
   in the middle of an emoji.
2. Emoji-heavy messages get previews far shorter than the 20 character
   limit. A message of 25 party popper emoji produces a preview of
   about 10 emoji, when 20 should fit.

Expected: the preview holds up to 20 characters, never splits an emoji,
and appends the ellipsis only when something was cut.

Actual: emoji get sliced in half and the limit counts them double.
