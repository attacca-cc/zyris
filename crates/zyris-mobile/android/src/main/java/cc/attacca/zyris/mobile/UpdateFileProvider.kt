package cc.attacca.zyris.mobile

import androidx.core.content.FileProvider

/**
 * Shares a downloaded update with the system installer. Its own class, because the app already
 * declares `androidx.core.content.FileProvider` and a manifest cannot name one class twice.
 */
class UpdateFileProvider : FileProvider()
