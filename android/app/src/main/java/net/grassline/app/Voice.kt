package net.grassline.app

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.DataOutputStream
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.UUID

/** Records a voice message as a 16 kHz mono WAV file in the cache. */
class VoiceRecorder(private val context: Context) {
    private var recorder: AudioRecord? = null
    private var thread: Thread? = null
    private var file: File? = null
    private var pcm: File? = null

    @Volatile
    private var recording = false

    /** Starts recording; throws when the microphone cannot be used. */
    fun start() {
        cancel()
        val wav = File.createTempFile("voice", ".wav", context.cacheDir)
        val raw = File.createTempFile("voice", ".pcm", context.cacheDir)
        val min = AudioRecord.getMinBufferSize(
            16_000,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        val bufferSize = maxOf(min, 16_000)
        val r = AudioRecord.Builder()
            .setAudioSource(MediaRecorder.AudioSource.MIC)
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(16_000)
                    .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                    .build()
            )
            .setBufferSizeInBytes(bufferSize)
            .build()
        if (r.state != AudioRecord.STATE_INITIALIZED) {
            r.release()
            wav.delete()
            raw.delete()
            throw IOException("the microphone is not available")
        }
        try {
            r.startRecording()
        } catch (e: Exception) {
            r.release()
            wav.delete()
            raw.delete()
            throw e
        }
        recording = true
        val t = Thread {
            try {
                raw.outputStream().buffered().use { out ->
                    val buffer = ByteArray(bufferSize)
                    while (recording) {
                        val n = r.read(buffer, 0, buffer.size)
                        if (n > 0) out.write(buffer, 0, n)
                    }
                }
            } catch (_: Exception) {
                // stop() reports what can be reported; a short file is still usable.
            }
        }
        recorder = r
        thread = t
        file = wav
        pcm = raw
        t.start()
    }

    /** Stops recording; returns the WAV file, or null when nothing was recorded. */
    fun stop(): File? {
        if (!recording) return null
        recording = false
        // stop() unblocks a read() that is waiting for the microphone.
        recorder?.let { runCatching { it.stop() } }
        thread?.let { runCatching { it.join(2_000) } }
        thread = null
        recorder?.release()
        recorder = null
        val raw = pcm
        val out = file
        pcm = null
        file = null
        if (raw == null || out == null) return null
        return try {
            if (raw.length() == 0L) {
                out.delete()
                null
            } else {
                writeWav(raw, out)
                out
            }
        } catch (e: Exception) {
            out.delete()
            null
        } finally {
            raw.delete()
        }
    }

    fun cancel() {
        stop()?.delete()
    }

    private fun writeWav(pcm: File, wav: File) {
        val bytes = pcm.length().toInt()
        DataOutputStream(wav.outputStream().buffered()).use { out ->
            out.writeBytes("RIFF")
            out.le32(36 + bytes)
            out.writeBytes("WAVEfmt ")
            out.le32(16)
            out.le16(1) // PCM
            out.le16(1) // mono
            out.le32(16_000)
            out.le32(16_000 * 2)
            out.le16(2)
            out.le16(16)
            out.writeBytes("data")
            out.le32(bytes)
            pcm.inputStream().use { it.copyTo(out) }
        }
    }
}

private fun DataOutputStream.le16(v: Int) {
    write(v and 0xff)
    write((v ushr 8) and 0xff)
}

private fun DataOutputStream.le32(v: Int) {
    write(v and 0xff)
    write((v ushr 8) and 0xff)
    write((v ushr 16) and 0xff)
    write((v ushr 24) and 0xff)
}

/** Turns [audio] into text with the daemon's local model.
 *  [onUploaded] tells when the audio is sent and the answer is being waited for. */
suspend fun transcribeLocal(audio: File, onUploaded: () -> Unit = {}): String = withContext(Dispatchers.IO) {
    onUploaded()
    val bytes = audio.readBytes()
    RustBridge.transcribe(bytes) ?: throw IOException("speech to text: the daemon did not answer")
}

/** Turns [audio] into text with an OpenAI-style `audio/transcriptions` endpoint.
 *  [onUploaded] tells when the audio is sent and the answer is being waited for. */
suspend fun transcribe(t: Transcription, audio: File, onUploaded: () -> Unit = {}): String = withContext(Dispatchers.IO) {
    val boundary = "grassline-" + UUID.randomUUID()
    val conn = URL(t.url).openConnection() as HttpURLConnection
    try {
        conn.requestMethod = "POST"
        conn.doOutput = true
        conn.connectTimeout = 15_000
        conn.readTimeout = 120_000
        conn.setRequestProperty("Authorization", "Bearer ${t.key}")
        conn.setRequestProperty("Content-Type", "multipart/form-data; boundary=$boundary")
        conn.outputStream.buffered().use { out ->
            fun line(s: String = "") = out.write((s + "\r\n").toByteArray())
            line("--$boundary")
            line("Content-Disposition: form-data; name=\"model\"")
            line()
            line(t.model)
            line("--$boundary")
            line("Content-Disposition: form-data; name=\"file\"; filename=\"voice.wav\"")
            line("Content-Type: audio/wav")
            line()
            audio.inputStream().use { it.copyTo(out) }
            line()
            line("--$boundary--")
        }
        onUploaded()
        val code = conn.responseCode
        val body = (if (code in 200..299) conn.inputStream else conn.errorStream)?.bufferedReader()?.use { it.readText() }.orEmpty()
        val json = runCatching { JSONObject(body) }.getOrNull()
        if (code !in 200..299) {
            // OpenAI and its look-alikes: {"error": {"message": …}}; others say it some other way.
            val why = json?.optJSONObject("error")?.optString("message")?.takeIf { it.isNotBlank() }
                ?: json?.optString("message")?.takeIf { it.isNotBlank() }
                ?: body.take(200).ifBlank { conn.responseMessage }
            throw IOException("speech to text: $code $why")
        }
        json?.optString("text") ?: throw IOException("speech to text: unexpected answer: ${body.take(200)}")
    } finally {
        conn.disconnect()
    }
}

enum class VoicePhase { Idle, Recording, Uploading, Transcribing }

/** A voice message on its way: recorded, then turned into text for [onText]. */
class VoiceState(private val id: String, private val context: Context, private val scope: CoroutineScope, private val onText: (String) -> Unit) {
    private val recorder = VoiceRecorder(context)
    var phase by mutableStateOf(VoicePhase.Idle)
        private set
    /** When the recording started. */
    var started by mutableLongStateOf(0L)
        private set
    /** What went wrong last. */
    var error by mutableStateOf<String?>(null)
    internal var askPermission: () -> Unit = {}
    /** A chat shows it; without one, it is dropped once its upload is over. */
    internal var shown = false

    /** It is on its way to the agent, so leaving the chat must not drop it. */
    val busy: Boolean get() = phase == VoicePhase.Uploading || phase == VoicePhase.Transcribing

    fun start() {
        error = null
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) begin()
        else askPermission()
    }

    internal fun begin() {
        try {
            recorder.start()
            started = System.currentTimeMillis()
            phase = VoicePhase.Recording
        } catch (e: Exception) {
            error = "cannot record: ${e.message ?: e}"
        }
    }

    fun cancel() {
        recorder.cancel()
        if (phase == VoicePhase.Recording) phase = VoicePhase.Idle
    }

    /** Stops recording and sends what was said, once [t] turned it into text. */
    fun finish(t: Transcription) {
        val audio = recorder.stop()
        if (audio == null) {
            phase = VoicePhase.Idle
            error = "nothing was recorded"
            return
        }
        phase = VoicePhase.Uploading
        scope.launch {
            try {
                val setTranscribing: () -> Unit = {
                    // The service does its work now: the send button says so.
                    scope.launch { if (phase == VoicePhase.Uploading) phase = VoicePhase.Transcribing }
                    Unit
                }
                val text = if (t.provider == "local") {
                    transcribeLocal(audio, setTranscribing)
                } else {
                    transcribe(t, audio, setTranscribing)
                }.trim()
                if (text.isEmpty()) error = "no speech was heard" else onText(text)
            } catch (e: Exception) {
                error = e.message ?: e.toString()
            } finally {
                audio.delete()
                phase = VoicePhase.Idle
                if (!shown) voices.remove(id)
            }
        }
    }
}

/** Voice messages still on their way, by session: they go on while the app shows other screens. */
private val voices = HashMap<String, VoiceState>()
/** Outlives the chat that started an upload. */
private val voiceScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

/** Whether session [id] has a voice message that has not reached the agent yet. */
fun voiceBusy(id: String): Boolean = voices[id]?.busy == true

@Composable
fun rememberVoice(sessionId: String, onText: (String) -> Unit): VoiceState {
    val context = LocalContext.current.applicationContext
    val send by rememberUpdatedState(onText)
    val voice = remember(sessionId) {
        voices.getOrPut(sessionId) { VoiceState(sessionId, context, voiceScope) { send(it) } }
    }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) voice.begin() else voice.error = "Grassline may not use the microphone."
    }
    voice.askPermission = { ask.launch(Manifest.permission.RECORD_AUDIO) }
    DisposableEffect(sessionId) {
        voice.shown = true
        onDispose {
            voice.cancel()
            voice.shown = false
            // An upload that is already going keeps its state until it is there.
            if (!voice.busy) voices.remove(sessionId)
        }
    }
    // A locked phone stops the app from hearing the microphone: keep the screen on while recording.
    val view = LocalView.current
    val recording = voice.phase == VoicePhase.Recording
    DisposableEffect(recording) {
        view.keepScreenOn = recording
        onDispose { view.keepScreenOn = false }
    }
    return voice
}

/** A spinner and what it waits for, in place of the send button. */
@Composable
fun BusyLabel(text: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
        Spacer(Modifier.width(8.dp))
        Text(text, style = MaterialTheme.typography.labelLarge)
    }
}

/** How long it has been recording, as m:ss. */
@Composable
fun RecordingTime(since: Long, modifier: Modifier = Modifier) {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(since) {
        while (true) {
            now = System.currentTimeMillis()
            delay(250)
        }
    }
    val s = ((now - since) / 1000).coerceAtLeast(0)
    Text("● Recording %d:%02d".format(s / 60, s % 60), modifier, color = MaterialTheme.colorScheme.error)
}
