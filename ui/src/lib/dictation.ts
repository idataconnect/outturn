import { useMemo } from 'react'
import { WebSpeechDictationAdapter } from '@assistant-ui/react'

import { useSession } from './session'

/**
 * The browser's own speech recognition, where this workspace allows it and
 * the browser has it. Undefined otherwise, which is also what tells the
 * composer to draw no microphone.
 *
 * In the language the browser is set to, which is the reader's rather than a
 * guess at the workspace's.
 */
export function useDictation(): WebSpeechDictationAdapter | undefined {
  const state = useSession()
  const allowed =
    state.status === 'authenticated' &&
    state.session.dictation !== false &&
    WebSpeechDictationAdapter.isSupported()
  return useMemo(
    () => (allowed ? new WebSpeechDictationAdapter({ language: navigator.language }) : undefined),
    [allowed],
  )
}
