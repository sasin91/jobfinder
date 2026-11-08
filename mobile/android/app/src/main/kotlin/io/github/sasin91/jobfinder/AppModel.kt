package io.github.sasin91.jobfinder

import android.app.Application
import android.os.Build
import android.provider.Settings
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.github.sasin91.jobfinder.protocol.DeviceClient
import io.github.sasin91.jobfinder.protocol.DeviceError
import io.github.sasin91.jobfinder.protocol.EVALUATION_WORKING
import io.github.sasin91.jobfinder.protocol.Evaluation
import io.github.sasin91.jobfinder.protocol.ListName
import io.github.sasin91.jobfinder.protocol.MAX_EVALUATE_CHARS
import io.github.sasin91.jobfinder.protocol.MAX_EVALUATE_HTML
import io.github.sasin91.jobfinder.protocol.isLink
import io.github.sasin91.jobfinder.protocol.MatchDetails
import io.github.sasin91.jobfinder.protocol.MatchRow
import io.github.sasin91.jobfinder.protocol.PairingLink
import io.github.sasin91.jobfinder.protocol.SHORTLIST
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** How often the posts sent are asked about while one is being matched, in milliseconds. */
private const val EVALUATIONS_POLL_MS = 2_000L

/**
 * Whether a link opens in the app's reader: an https link. The app refuses plain http (its
 * network security config), so an http link goes to the computer, which reads it itself.
 */
private fun opensInApp(text: String) = isLink(text) && text.trim().lowercase().startsWith("https://")

/** What the app shows. */
sealed interface Screen {
    data object Matches : Screen
    data class Match(val id: Long) : Screen
    data object Send : Screen
    data object Scan : Screen

    /** A shared or pasted link, shown in the app so the page goes to the computer as shown. */
    data class Read(val url: String) : Screen
}

data class PairState(
    val linkText: String = "",
    val busy: Boolean = false,
    val error: String? = null,
)

data class ListState(
    val lists: List<ListName> = emptyList(),
    val list: String = SHORTLIST,
    val rows: List<MatchRow> = emptyList(),
    val nextAfter: String? = null,
    val loading: Boolean = false,
    val loaded: Boolean = false,
    val error: String? = null,
)

data class MatchState(
    val id: Long? = null,
    val details: MatchDetails? = null,
    val loading: Boolean = false,
    val error: String? = null,
)

data class SendState(
    val text: String = "",
    val sending: Boolean = false,
    val error: String? = null,
    val evaluations: List<Evaluation> = emptyList(),
)

/**
 * The app's state, and the calls to JobFinder on the computer. Errors are shown where they
 * happened; a token the computer no longer knows (the device was removed there) unpairs.
 */
class AppModel(application: Application) : AndroidViewModel(application) {
    private val store = PairingStore(application)

    private val _pairing = MutableStateFlow(store.read())
    val pairing: StateFlow<Pairing?> = _pairing.asStateFlow()

    private val _screen = MutableStateFlow<Screen>(Screen.Matches)
    val screen: StateFlow<Screen> = _screen.asStateFlow()

    private val _pair = MutableStateFlow(PairState())
    val pair: StateFlow<PairState> = _pair.asStateFlow()

    private val _list = MutableStateFlow(ListState())
    val list: StateFlow<ListState> = _list.asStateFlow()

    private val _match = MutableStateFlow(MatchState())
    val match: StateFlow<MatchState> = _match.asStateFlow()

    private val _send = MutableStateFlow(SendState())
    val send: StateFlow<SendState> = _send.asStateFlow()

    /**
     * Where the requests to the computer paired now run. Unpairing or pairing again cancels it,
     * so a late answer from before can't fill in or unpair the new pairing.
     */
    private var session = newSession()

    private var polling: Job? = null

    /** The list request running now; choosing another list or refreshing supersedes it. */
    private var listing: Job? = null

    private fun newSession() = CoroutineScope(viewModelScope.coroutineContext + SupervisorJob(viewModelScope.coroutineContext[Job]))

    private fun restartSession() {
        session.cancel()
        session = newSession()
        polling = null
        listing = null
        // What was read or sent through the pairing before belongs to it; a draft post stays.
        _list.value = ListState()
        _match.value = MatchState()
        _send.update { SendState(text = it.text) }
    }

    private fun client(): DeviceClient? = _pairing.value?.let { DeviceClient(it.address, it.token) }

    fun show(screen: Screen) {
        _screen.value = screen
        when (screen) {
            is Screen.Match -> if (_match.value.id != screen.id || _match.value.error != null) loadMatch(screen.id)
            Screen.Matches -> if (!_list.value.loaded) refresh()
            Screen.Send -> refreshEvaluations()
            Screen.Scan, is Screen.Read -> Unit
        }
    }

    /** Back from where the app is; false when there is nowhere to go back to. */
    fun back(): Boolean {
        if (_screen.value == Screen.Matches) return false
        _screen.value = if (_screen.value is Screen.Read) Screen.Send else Screen.Matches
        return true
    }

    /** Once paired: the list, unless it is read already; and what was sent, when shown. */
    fun loadFirst() {
        if (!_list.value.loaded) refresh()
        if (_screen.value == Screen.Send) refreshEvaluations()
    }

    // Pairing

    fun typeLink(text: String) = _pair.update { it.copy(linkText = text, error = null) }

    /** Pairs with the computer the pairing link in text points to. */
    fun pairWith(text: String) {
        val link = PairingLink.parse(text)
        if (link == null) {
            _pair.update { it.copy(error = "That isn't a JobFinder pairing code. On the computer, open Settings, Devices, and show a new one.") }
            _screen.value = Screen.Matches
            return
        }
        _screen.value = Screen.Matches
        _pair.update { it.copy(linkText = text.trim(), busy = true, error = null) }
        restartSession()
        session.launch {
            try {
                val answer = DeviceClient(link.address).pair(link, deviceName())
                val pairing = Pairing(link.address, answer.token)
                store.save(pairing)
                _pairing.value = pairing
                _pair.value = PairState()
                _list.value = ListState()
                refresh()
                // A post shared before pairing waits in the box: go on to send it.
                if (_send.value.text.isNotBlank()) show(Screen.Send)
            } catch (e: DeviceError) {
                _pair.update { it.copy(busy = false, error = e.message) }
            }
        }
    }

    /** Forgets the pairing on this device. The computer lists it until it is removed there. */
    fun unpair() {
        restartSession()
        store.forget()
        _pairing.value = null
        _list.value = ListState()
        _match.value = MatchState()
        _send.value = SendState()
        _screen.value = Screen.Matches
    }

    private fun deviceName(): String {
        val app = getApplication<Application>()
        return Settings.Global.getString(app.contentResolver, Settings.Global.DEVICE_NAME)
            ?.takeIf { it.isNotBlank() }
            ?: "${Build.MANUFACTURER} ${Build.MODEL}"
    }

    private fun failed(e: DeviceError, show: (String) -> Unit) {
        if (e.notPaired) {
            unpair()
            _pair.update { it.copy(error = e.message) }
        } else {
            show(e.message ?: "Something went wrong.")
        }
    }

    // Lists

    fun chooseList(name: String) {
        _list.update { it.copy(list = name, rows = emptyList(), nextAfter = null) }
        loadList(after = "")
    }

    fun refresh() = loadList(after = "")

    fun loadMore() {
        if (listing?.isActive == true) return
        val after = _list.value.nextAfter ?: return
        loadList(after)
    }

    /** Reads a batch of the list chosen now; a first batch supersedes the request running. */
    private fun loadList(after: String) {
        val client = client() ?: return
        listing?.cancel()
        val wanted = _list.value.list
        _list.update { it.copy(loading = true, error = null) }
        listing = session.launch {
            try {
                val found = client.matches(wanted, after)
                _list.update {
                    if (it.list != wanted) return@update it
                    it.copy(
                        lists = found.lists,
                        list = found.list,
                        rows = if (after.isEmpty()) found.matches else it.rows + found.matches,
                        nextAfter = found.nextAfter,
                        loading = false,
                        loaded = true,
                    )
                }
            } catch (e: DeviceError) {
                _list.update { it.copy(loading = false) }
                failed(e) { why -> _list.update { it.copy(error = why) } }
            }
        }
    }

    // A match

    fun loadMatch(id: Long) {
        val client = client() ?: return
        _match.value = MatchState(id = id, loading = true)
        session.launch {
            try {
                val details = client.match(id)
                _match.update { if (it.id == id) it.copy(details = details, loading = false) else it }
            } catch (e: DeviceError) {
                _match.update { it.copy(loading = false) }
                failed(e) { why -> _match.update { if (it.id == id) it.copy(error = why) else it } }
            }
        }
    }

    // Sending posts

    fun typePost(text: String) = _send.update { it.copy(text = text, error = null) }

    /**
     * A post shared from another app: shown in the box to send, and a link opens to be read. The
     * link stays in the box too, so it isn't lost when the device has to pair first (the pairing
     * screen shows instead of the reader) or the reader is left.
     */
    fun shared(text: String) {
        val trimmed = text.trim()
        _send.update { it.copy(text = trimmed.take(MAX_EVALUATE_CHARS), error = null) }
        if (opensInApp(trimmed) && _pairing.value != null) {
            show(Screen.Read(trimmed))
            return
        }
        show(Screen.Send)
    }

    /** Sends the text in the box; a link in it opens first, to be sent as its page. */
    fun sendPost() {
        val text = _send.value.text.trim()
        if (text.isEmpty()) {
            _send.update { it.copy(error = "Paste or share a link to a job post, or its text.") }
            return
        }
        if (opensInApp(text)) {
            // The link stays in the box until it is sent, to try again if sending fails.
            _send.update { it.copy(error = null) }
            show(Screen.Read(text))
            return
        }
        send(text) { client -> client.evaluate(text) }
    }

    /** Sends the page at url as the app showed it. */
    fun sendPage(url: String, html: String) {
        if (html.length > MAX_EVALUATE_HTML) {
            _send.update { it.copy(text = url, error = "That page is too large to send. Send its link instead.") }
            show(Screen.Send)
            return
        }
        val draft = readerDraft(url)
        show(Screen.Send)
        send(draft) { client -> client.evaluatePage(url, html) }
    }

    /** Sends only the link: the computer reads the page itself. */
    fun sendLink(url: String) {
        val draft = readerDraft(url)
        show(Screen.Send)
        send(draft) { client -> client.evaluate(url) }
    }

    /**
     * The link the reader was opened with, which is what the box holds: the page sent from it may
     * be at another url (a short link or a tracking link that redirected).
     */
    private fun readerDraft(url: String): String = (_screen.value as? Screen.Read)?.url ?: url

    /** Sends what start sends, noting it on the Send screen; draft is what the box held. */
    private fun send(draft: String, start: suspend (DeviceClient) -> Evaluation) {
        val client = client() ?: return
        _send.update { it.copy(sending = true, error = null) }
        session.launch {
            try {
                val started = start(client)
                _send.update { state ->
                    state.copy(
                        // A post shared while this one was sent stays in the box.
                        text = if (state.text.trim() == draft) "" else state.text,
                        sending = false,
                        evaluations = listOf(started) + state.evaluations.filter { it.id != started.id },
                    )
                }
                pollWhileWorking()
            } catch (e: DeviceError) {
                // What wasn't sent goes back in an empty box (a shared link), to try again.
                _send.update { it.copy(sending = false, text = it.text.ifBlank { draft }) }
                failed(e) { why -> _send.update { it.copy(error = why) } }
            }
        }
    }

    fun refreshEvaluations() {
        val client = client() ?: return
        session.launch {
            try {
                val evaluations = client.evaluations().evaluations
                _send.update { it.copy(evaluations = evaluations) }
                pollWhileWorking()
            } catch (e: DeviceError) {
                failed(e) { why -> _send.update { it.copy(error = why) } }
            }
        }
    }

    /** Asks about the posts sent every EVALUATIONS_POLL_MS while one of them is being matched. */
    private fun pollWhileWorking() {
        if (polling?.isActive == true) return
        if (_send.value.evaluations.none { it.state == EVALUATION_WORKING }) return
        polling = session.launch {
            while (_send.value.evaluations.any { it.state == EVALUATION_WORKING }) {
                delay(EVALUATIONS_POLL_MS)
                val client = client() ?: break
                try {
                    val evaluations = client.evaluations().evaluations
                    _send.update { it.copy(evaluations = evaluations) }
                } catch (e: DeviceError) {
                    failed(e) { why -> _send.update { it.copy(error = why) } }
                    break
                }
            }
            // A finished match is in the lists now.
            refresh()
        }
    }
}
