import { logoUrl, productName } from '../lib/brand'

/**
 * The deployment's mark.
 *
 * A component rather than six `<img src="/favicon.svg">`, so a deployer
 * replaces one file and one variable instead of finding every place the old
 * path was written out.
 *
 * Decorative by default: the wordmark beside it already names the product, and
 * a screen reader that reads the name twice is worse than one that reads it
 * once. Pass `labelled` where the mark stands alone.
 */
export default function Logo({
  className = 'w-6 h-6 shrink-0',
  labelled = false,
}: {
  className?: string
  labelled?: boolean
}) {
  return (
    <img
      src={logoUrl}
      alt={labelled ? productName : ''}
      aria-hidden={labelled ? undefined : true}
      className={className}
    />
  )
}
