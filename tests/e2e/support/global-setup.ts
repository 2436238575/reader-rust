import { startServerIfNeeded } from './server'

export default async function globalSetup() {
  await startServerIfNeeded()
}
