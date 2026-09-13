/**
 * Who this deployment says it is.
 *
 * outturn is Apache-2.0 and expects to be run by companies serving their own
 * customers under their own name. That makes the product name and the mark
 * configuration, not source: a deployer who has to edit JSX to change a
 * wordmark carries a patch on a hot file forever, and every upgrade is a
 * merge conflict on the one line they changed.
 *
 * Read from the build environment rather than from the API, because these are
 * properties of the deployment rather than of the workspace being viewed --
 * the login page needs them before anyone is signed in, and a wordmark that
 * arrives a frame late is a wordmark that flickers.
 */

/** The product name, as a person reads it. */
export const productName = import.meta.env.VITE_BRAND_NAME ?? 'outturn'

/**
 * The mark. One constant rather than six copies of a path, so replacing it is
 * replacing a file and not finding every `<img>` that pointed at the old one.
 */
export const logoUrl = import.meta.env.VITE_BRAND_LOGO ?? '/favicon.svg'

/** Shown where the name needs a sentence around it, as on the login page. */
export const productTagline = import.meta.env.VITE_BRAND_TAGLINE ?? null
