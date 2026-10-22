import { stopServerIfStarted } from './server'

export default async function globalTeardown() {
  await stopServerIfStarted()
}
