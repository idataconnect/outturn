import { useMemo } from 'react'
import { WebSpeechDictationAdapter, type DictationEnd } from '@assistant-ui/react'

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

/**
 * What to tell somebody whose dictation ended on an error, from the code the
 * browser's speech recognition gave. Undefined for an ending that needs no
 * explanation: one they stopped themselves, or no error at all.
 *
 * The codes are the Web Speech API's. Anything unlisted -- a future code, or
 * an adapter that gives none -- still gets a sentence, because a microphone
 * that stops for no stated reason is what this exists to prevent.
 */
export function dictationFailure(end: DictationEnd | undefined): string | undefined {
  if (end?.reason !== 'error') return undefined
  switch (end.error?.code) {
    case 'not-allowed':
    case 'service-not-allowed':
      return 'Microphone access is blocked for this site.'
    case 'audio-capture':
      return 'No microphone was found.'
    case 'no-speech':
      return 'No speech was heard.'
    case 'network':
      return 'Speech recognition could not reach its service.'
    case 'language-not-supported':
      return "Speech recognition does not support this browser's language."
    default:
      return 'Dictation stopped because of an error.'
  }
}
