/// The place you drag the button to put it away.
///
/// Purely something to look at: the window ignores the cursor entirely, and
/// whether the button is over it is worked out from the two windows' real
/// positions rather than by this one receiving anything.
export function TouchTarget() {
  return (
    <div className="target">
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M6 6l12 12M18 6L6 18" />
      </svg>
    </div>
  );
}
