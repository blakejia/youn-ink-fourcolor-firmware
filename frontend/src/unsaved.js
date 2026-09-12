// A page that holds unsaved edits registers a predicate here, so chrome that can
// discard that work (the device selector) can ask before doing so.
let check = null;

export function registerUnsavedCheck(fn) {
  check = fn;
}

export function hasUnsaved() {
  return check ? !!check() : false;
}
