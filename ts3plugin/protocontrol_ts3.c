/*
 * Proto-Control TeamSpeak 3 Plugin
 *
 * Exposes per-user volume/mute control via Unix domain socket IPC.
 * The plugin acts as socket server; the rotocontrol daemon connects as client.
 *
 * IPC protocol (newline-delimited JSON):
 *   Plugin → daemon: {"type":"members","members":[{"id":N,"nick":"...","muted":bool},...]}
 *   Daemon → plugin: {"type":"set_volume","client_id":N,"volume":V}  (V: 0-200, 100 = 0 dB)
 *   Daemon → plugin: {"type":"set_mute","client_id":N,"muted":bool}
 *
 * Volume mapping: dB = (volume - 100) * 0.4  →  range -40 dB … +40 dB
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <pthread.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <poll.h>
#include <errno.h>

#include "teamspeak/public_definitions.h"
#include "teamspeak/public_rare_definitions.h"
#include "ts3_functions.h"
#include "plugin_definitions.h"

#define PLUGIN_API_VERSION 26
#define PLUGINS_EXPORTDLL __attribute__((visibility("default")))

static struct TS3Functions ts3;

static char     socket_path[108]; /* sizeof(sockaddr_un.sun_path) on Linux */
static int      server_fd  = -1;
static int      client_fd  = -1;
static pthread_t accept_tid;
static pthread_t cmd_tid;
static pthread_mutex_t client_mu = PTHREAD_MUTEX_INITIALIZER;
static volatile int running = 0;

/* Track which clients WE have explicitly muted (not auto-muted by TS3). */
#define MAX_MUTED 128
static anyID manually_muted[MAX_MUTED];
static int   manually_muted_count = 0;
static pthread_mutex_t muted_mu = PTHREAD_MUTEX_INITIALIZER;

static int is_manually_muted(anyID cid) {
    for (int i = 0; i < manually_muted_count; i++)
        if (manually_muted[i] == cid) return 1;
    return 0;
}
static void set_manually_muted(anyID cid, int mute) {
    pthread_mutex_lock(&muted_mu);
    if (mute) {
        if (!is_manually_muted(cid) && manually_muted_count < MAX_MUTED)
            manually_muted[manually_muted_count++] = cid;
    } else {
        for (int i = 0; i < manually_muted_count; i++) {
            if (manually_muted[i] == cid) {
                manually_muted[i] = manually_muted[--manually_muted_count];
                break;
            }
        }
    }
    pthread_mutex_unlock(&muted_mu);
}

/* ---- helpers ---- */

static void write_to_client(const char *buf, size_t len) {
    pthread_mutex_lock(&client_mu);
    int fd = client_fd;
    pthread_mutex_unlock(&client_mu);
    if (fd >= 0) { ssize_t _ignored = write(fd, buf, len); (void)_ignored; }
}

/* Escape a UTF-8 string for JSON (handles " and \) */
static void json_escape(const char *src, char *dst, size_t dstlen) {
    size_t j = 0;
    for (size_t i = 0; src[i] && j + 3 < dstlen; i++) {
        unsigned char c = (unsigned char)src[i];
        if (c == '"' || c == '\\') dst[j++] = '\\';
        dst[j++] = (char)c;
    }
    dst[j] = '\0';
}

static uint64 get_server(void) {
    uint64 *servers = NULL;
    if (ts3.getServerConnectionHandlerList(&servers) != 0 || !servers || !servers[0]) {
        if (servers) ts3.freeMemory(servers);
        return 0;
    }
    uint64 id = servers[0];
    ts3.freeMemory(servers);
    return id;
}

static void send_members(uint64 server_id) {
    anyID my_id = 0;
    if (ts3.getClientID(server_id, &my_id) != 0) return;

    uint64 my_channel = 0;
    if (ts3.getChannelOfClient(server_id, my_id, &my_channel) != 0 || my_channel == 0) {
        write_to_client("{\"type\":\"members\",\"members\":[]}\n", 31);
        return;
    }

    anyID *clients = NULL;
    if (ts3.getChannelClientList(server_id, my_channel, &clients) != 0) {
        write_to_client("{\"type\":\"members\",\"members\":[]}\n", 31);
        return;
    }

    char buf[4096];
    int pos = snprintf(buf, sizeof(buf), "{\"type\":\"members\",\"members\":[");
    int first = 1;

    for (int i = 0; clients[i] != 0 && pos < (int)sizeof(buf) - 128; i++) {
        anyID cid = clients[i];
        if (cid == my_id) continue;

        char *raw_nick = NULL;
        ts3.getClientVariableAsString(server_id, cid, CLIENT_NICKNAME, &raw_nick);
        int self_mic_muted = 0, self_deafened = 0;
        ts3.getClientVariableAsInt(server_id, cid, CLIENT_INPUT_MUTED,  &self_mic_muted);
        ts3.getClientVariableAsInt(server_id, cid, CLIENT_OUTPUT_MUTED, &self_deafened);

        char nick[256] = "?";
        if (raw_nick) {
            json_escape(raw_nick, nick, sizeof(nick));
            ts3.freeMemory(raw_nick);
        }

        pthread_mutex_lock(&muted_mu);
        int muted = is_manually_muted(cid);
        pthread_mutex_unlock(&muted_mu);

        if (!first) pos += snprintf(buf + pos, sizeof(buf) - pos, ",");
        pos += snprintf(buf + pos, sizeof(buf) - pos,
            "{\"id\":%u,\"nick\":\"%s\",\"muted\":%s,\"self_muted\":%s,\"self_deafened\":%s}",
            (unsigned)cid, nick,
            muted           ? "true" : "false",
            self_mic_muted  ? "true" : "false",
            self_deafened   ? "true" : "false");
        first = 0;
    }

    ts3.freeMemory(clients);
    pos += snprintf(buf + pos, sizeof(buf) - pos, "]}\n");
    write_to_client(buf, (size_t)pos);
}

/* ---- command processing thread ---- */

static void *cmd_thread(void *arg) {
    (void)arg;
    char line[512];
    int pos = 0;

    while (running) {
        pthread_mutex_lock(&client_mu);
        int fd = client_fd;
        pthread_mutex_unlock(&client_mu);

        if (fd < 0) {
            usleep(50000);
            pos = 0;
            continue;
        }

        struct pollfd pfd2 = { .fd = fd, .events = POLLIN };
        if (poll(&pfd2, 1, 200) <= 0 || !running) continue;

        char ch;
        ssize_t n = read(fd, &ch, 1);
        if (n <= 0) {
            pthread_mutex_lock(&client_mu);
            if (client_fd == fd) {
                close(client_fd);
                client_fd = -1;
            }
            pthread_mutex_unlock(&client_mu);
            pos = 0;
            continue;
        }

        if (ch == '\n' || pos >= (int)sizeof(line) - 1) {
            line[pos] = '\0';
            pos = 0;

            uint64 server_id = get_server();
            if (!server_id || !line[0]) continue;

            char *p_cid = strstr(line, "\"client_id\":");
            if (!p_cid) continue;
            unsigned cid = 0;
            sscanf(p_cid + 12, "%u", &cid);

            if (strstr(line, "\"set_volume\"")) {
                char *p_vol = strstr(line, "\"volume\":");
                if (p_vol) {
                    int vol = 100;
                    sscanf(p_vol + 9, "%d", &vol);
                    float db = (vol - 100.0f) * 0.4f;
                    ts3.setClientVolumeModifier(server_id, (anyID)cid, db);
                }
            } else if (strstr(line, "\"set_mute\"")) {
                int muted = strstr(line, "\"muted\":true") != NULL;
                anyID ids[2] = { (anyID)cid, 0 };
                set_manually_muted((anyID)cid, muted);
                if (muted) {
                    ts3.requestMuteClients(server_id, ids, NULL);
                } else {
                    ts3.requestUnmuteClients(server_id, ids, NULL);
                }
            }
        } else {
            line[pos++] = ch;
        }
    }
    return NULL;
}

/* ---- accept thread ---- */

static void *accept_thread(void *arg) {
    (void)arg;
    while (running) {
        /* Poll with timeout so we can check 'running' without blocking forever. */
        struct pollfd pfd = { .fd = server_fd, .events = POLLIN };
        if (poll(&pfd, 1, 200) <= 0 || !running) continue;

        int fd = accept(server_fd, NULL, NULL);
        if (fd < 0) continue;

        pthread_mutex_lock(&client_mu);
        if (client_fd >= 0) close(client_fd);
        client_fd = fd;
        pthread_mutex_unlock(&client_mu);

        uint64 server_id = get_server();
        if (server_id) {
            send_members(server_id);
        } else {
            write_to_client("{\"type\":\"members\",\"members\":[]}\n", 31);
        }
    }
    return NULL;
}

/* ======== Required plugin exports ======== */

PLUGINS_EXPORTDLL const char *ts3plugin_name()        { return "Roto-Control"; }
PLUGINS_EXPORTDLL const char *ts3plugin_version()     { return "1.0.0"; }
PLUGINS_EXPORTDLL int          ts3plugin_apiVersion()  { return PLUGIN_API_VERSION; }
PLUGINS_EXPORTDLL const char *ts3plugin_author()      { return "protocontrol"; }
PLUGINS_EXPORTDLL const char *ts3plugin_description() {
    return "Hardware volume controller integration for Roto-Control.";
}
PLUGINS_EXPORTDLL void ts3plugin_setFunctionPointers(const struct TS3Functions funcs) {
    ts3 = funcs;
}

PLUGINS_EXPORTDLL int ts3plugin_init() {
    const char *home = getenv("HOME");
    snprintf(socket_path, sizeof(socket_path), "%s/.ts3client/protocontrol-ts3.sock",
             home ? home : "/tmp");

    server_fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (server_fd < 0) return 1;

    unlink(socket_path);

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    memcpy(addr.sun_path, socket_path, sizeof(addr.sun_path));

    if (bind(server_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(server_fd);
        server_fd = -1;
        return 1;
    }
    listen(server_fd, 1);

    running = 1;
    pthread_create(&accept_tid, NULL, accept_thread, NULL);
    pthread_create(&cmd_tid,    NULL, cmd_thread,    NULL);
    return 0;
}

PLUGINS_EXPORTDLL void ts3plugin_shutdown() {
    running = 0;
    /* shutdown() wakes threads blocked in accept()/read() before close(). */
    if (server_fd >= 0) { shutdown(server_fd, SHUT_RDWR); close(server_fd); server_fd = -1; }
    pthread_mutex_lock(&client_mu);
    if (client_fd >= 0) { shutdown(client_fd, SHUT_RDWR); close(client_fd); client_fd = -1; }
    pthread_mutex_unlock(&client_mu);
    pthread_join(accept_tid, NULL);
    pthread_join(cmd_tid,    NULL);
    unlink(socket_path);
}

/* ======== Optional plugin exports ======== */

PLUGINS_EXPORTDLL int  ts3plugin_offersConfigure() { return PLUGIN_OFFERS_NO_CONFIGURE; }
PLUGINS_EXPORTDLL void ts3plugin_configure(void *h, void *p) { (void)h; (void)p; }
PLUGINS_EXPORTDLL void ts3plugin_registerPluginID(const char *id) { (void)id; }
PLUGINS_EXPORTDLL const char *ts3plugin_commandKeyword() { return NULL; }
PLUGINS_EXPORTDLL int  ts3plugin_processCommand(uint64 s, const char *c) { (void)s; (void)c; return 0; }
PLUGINS_EXPORTDLL void ts3plugin_currentServerConnectionChanged(uint64 s) { (void)s; }
PLUGINS_EXPORTDLL const char *ts3plugin_infoTitle() { return NULL; }
PLUGINS_EXPORTDLL void ts3plugin_infoData(uint64 s, uint64 id, enum PluginItemType t, char **d) {
    (void)s; (void)id; (void)t; *d = NULL;
}
PLUGINS_EXPORTDLL void ts3plugin_freeMemory(void *d) { free(d); }
PLUGINS_EXPORTDLL int  ts3plugin_requestAutoload() { return 0; }
PLUGINS_EXPORTDLL void ts3plugin_initMenus(struct PluginMenuItem ***m, char **i) { *m = NULL; *i = NULL; }
PLUGINS_EXPORTDLL void ts3plugin_initHotkeys(struct PluginHotkey ***h) { *h = NULL; }

/* ======== Callbacks ======== */

PLUGINS_EXPORTDLL void ts3plugin_onConnectStatusChangeEvent(uint64 s, int status, unsigned int err) {
    (void)err;
    if (status == STATUS_CONNECTION_ESTABLISHED)
        send_members(s);
    else if (status == STATUS_DISCONNECTED)
        write_to_client("{\"type\":\"members\",\"members\":[]}\n", 31);
}

/* Client moves — all need member list refresh; leaving clients lose their mute entry */
PLUGINS_EXPORTDLL void ts3plugin_onClientMoveEvent(uint64 s, anyID c, uint64 oc, uint64 nc, int v, const char *m)
    { (void)oc;(void)nc;(void)v;(void)m; if (nc == 0) set_manually_muted(c, 0); send_members(s); }
PLUGINS_EXPORTDLL void ts3plugin_onClientMoveTimeoutEvent(uint64 s, anyID c, uint64 oc, uint64 nc, int v, const char *m)
    { (void)oc;(void)nc;(void)v;(void)m; set_manually_muted(c, 0); send_members(s); }
PLUGINS_EXPORTDLL void ts3plugin_onClientMoveMovedEvent(uint64 s, anyID c, uint64 oc, uint64 nc, int v, anyID mi, const char *mn, const char *mu, const char *m)
    { (void)oc;(void)nc;(void)v;(void)mi;(void)mn;(void)mu;(void)m; set_manually_muted(c, 0); send_members(s); }
PLUGINS_EXPORTDLL void ts3plugin_onClientKickFromChannelEvent(uint64 s, anyID c, uint64 oc, uint64 nc, int v, anyID ki, const char *kn, const char *ku, const char *m)
    { (void)oc;(void)nc;(void)v;(void)ki;(void)kn;(void)ku;(void)m; set_manually_muted(c, 0); send_members(s); }
PLUGINS_EXPORTDLL void ts3plugin_onClientKickFromServerEvent(uint64 s, anyID c, uint64 oc, uint64 nc, int v, anyID ki, const char *kn, const char *ku, const char *m)
    { (void)oc;(void)nc;(void)v;(void)ki;(void)kn;(void)ku;(void)m; set_manually_muted(c, 0); send_members(s); }
PLUGINS_EXPORTDLL void ts3plugin_onClientMoveSubscriptionEvent(uint64 s, anyID c, uint64 oc, uint64 nc, int v)
    { (void)c;(void)oc;(void)nc;(void)v; send_members(s); }
PLUGINS_EXPORTDLL void ts3plugin_onUpdateClientEvent(uint64 s, anyID c, anyID i, const char *n, const char *u)
    { (void)c;(void)i;(void)n;(void)u; send_members(s); }
PLUGINS_EXPORTDLL void ts3plugin_onClientBanFromServerEvent(uint64 s, anyID c, uint64 oc, uint64 nc, int v, anyID ki, const char *kn, const char *ku, uint64 t, const char *m)
    { (void)c;(void)oc;(void)nc;(void)v;(void)ki;(void)kn;(void)ku;(void)t;(void)m; send_members(s); }

/* Stubs — required by TS3 plugin API */
PLUGINS_EXPORTDLL void ts3plugin_onNewChannelEvent(uint64 s, uint64 c, uint64 p)                                     { (void)s;(void)c;(void)p; }
PLUGINS_EXPORTDLL void ts3plugin_onNewChannelCreatedEvent(uint64 s, uint64 c, uint64 p, anyID i, const char *n, const char *u)  { (void)s;(void)c;(void)p;(void)i;(void)n;(void)u; }
PLUGINS_EXPORTDLL void ts3plugin_onDelChannelEvent(uint64 s, uint64 c, anyID i, const char *n, const char *u)        { (void)s;(void)c;(void)i;(void)n;(void)u; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelMoveEvent(uint64 s, uint64 c, uint64 p, anyID i, const char *n, const char *u) { (void)s;(void)c;(void)p;(void)i;(void)n;(void)u; }
PLUGINS_EXPORTDLL void ts3plugin_onUpdateChannelEvent(uint64 s, uint64 c)                                            { (void)s;(void)c; }
PLUGINS_EXPORTDLL void ts3plugin_onUpdateChannelEditedEvent(uint64 s, uint64 c, anyID i, const char *n, const char *u) { (void)s;(void)c;(void)i;(void)n;(void)u; }
PLUGINS_EXPORTDLL void ts3plugin_onClientIDsEvent(uint64 s, const char *uid, anyID c, const char *n)                 { (void)s;(void)uid;(void)c;(void)n; }
PLUGINS_EXPORTDLL void ts3plugin_onClientIDsFinishedEvent(uint64 s)                                                  { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onServerEditedEvent(uint64 s, anyID e, const char *n, const char *u)                { (void)s;(void)e;(void)n;(void)u; }
PLUGINS_EXPORTDLL void ts3plugin_onServerUpdatedEvent(uint64 s)                                                      { (void)s; }
PLUGINS_EXPORTDLL int  ts3plugin_onServerErrorEvent(uint64 s, const char *m, unsigned int e, const char *r, const char *ex) { (void)s;(void)m;(void)e;(void)r;(void)ex; return 0; }
PLUGINS_EXPORTDLL void ts3plugin_onServerStopEvent(uint64 s, const char *m)                                          { (void)s;(void)m; }
PLUGINS_EXPORTDLL int  ts3plugin_onTextMessageEvent(uint64 s, anyID tt, anyID to, anyID fr, const char *fn, const char *fu, const char *m, int ff) { (void)s;(void)tt;(void)to;(void)fr;(void)fn;(void)fu;(void)m;(void)ff; return 0; }
PLUGINS_EXPORTDLL void ts3plugin_onTalkStatusChangeEvent(uint64 s, int st, int rw, anyID c)                          { (void)s;(void)st;(void)rw;(void)c; }
PLUGINS_EXPORTDLL void ts3plugin_onConnectionInfoEvent(uint64 s, anyID c)                                            { (void)s;(void)c; }
PLUGINS_EXPORTDLL void ts3plugin_onServerConnectionInfoEvent(uint64 s)                                               { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelSubscribeEvent(uint64 s, uint64 c)                                         { (void)s;(void)c; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelSubscribeFinishedEvent(uint64 s)                                           { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelUnsubscribeEvent(uint64 s, uint64 c)                                       { (void)s;(void)c; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelUnsubscribeFinishedEvent(uint64 s)                                         { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelDescriptionUpdateEvent(uint64 s, uint64 c)                                 { (void)s;(void)c; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelPasswordChangedEvent(uint64 s, uint64 c)                                   { (void)s;(void)c; }
PLUGINS_EXPORTDLL void ts3plugin_onPlaybackShutdownCompleteEvent(uint64 s)                                           { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onSoundDeviceListChangedEvent(const char *m, int p)                                 { (void)m;(void)p; }
PLUGINS_EXPORTDLL void ts3plugin_onEditPlaybackVoiceDataEvent(uint64 s, anyID c, short *sa, int sc, int ch)          { (void)s;(void)c;(void)sa;(void)sc;(void)ch; }
PLUGINS_EXPORTDLL void ts3plugin_onEditPostProcessVoiceDataEvent(uint64 s, anyID c, short *sa, int sc, int ch, const unsigned int *csa, unsigned int *cfm) { (void)s;(void)c;(void)sa;(void)sc;(void)ch;(void)csa;(void)cfm; }
PLUGINS_EXPORTDLL void ts3plugin_onEditMixedPlaybackVoiceDataEvent(uint64 s, short *sa, int sc, int ch, const unsigned int *csa, unsigned int *cfm) { (void)s;(void)sa;(void)sc;(void)ch;(void)csa;(void)cfm; }
PLUGINS_EXPORTDLL void ts3plugin_onEditCapturedVoiceDataEvent(uint64 s, short *sa, int sc, int ch, int *e)           { (void)s;(void)sa;(void)sc;(void)ch;(void)e; }
PLUGINS_EXPORTDLL void ts3plugin_onCustom3dRolloffCalculationClientEvent(uint64 s, anyID c, float d, float *v)       { (void)s;(void)c;(void)d;(void)v; }
PLUGINS_EXPORTDLL void ts3plugin_onCustom3dRolloffCalculationWaveEvent(uint64 s, uint64 w, float d, float *v)        { (void)s;(void)w;(void)d;(void)v; }
PLUGINS_EXPORTDLL void ts3plugin_onUserLoggingMessageEvent(const char *lm, int ll, const char *lc, uint64 lid, const char *lt, const char *cls) { (void)lm;(void)ll;(void)lc;(void)lid;(void)lt;(void)cls; }
PLUGINS_EXPORTDLL int  ts3plugin_onClientPokeEvent(uint64 s, anyID f, const char *pn, const char *pu, const char *m, int ff) { (void)s;(void)f;(void)pn;(void)pu;(void)m;(void)ff; return 0; }
PLUGINS_EXPORTDLL void ts3plugin_onClientSelfVariableUpdateEvent(uint64 s, int f, const char *ov, const char *nv)    { (void)s;(void)f;(void)ov;(void)nv; }
PLUGINS_EXPORTDLL void ts3plugin_onFileListEvent(uint64 s, uint64 c, const char *p, const char *n, uint64 sz, uint64 dt, int t, uint64 is, const char *rc) { (void)s;(void)c;(void)p;(void)n;(void)sz;(void)dt;(void)t;(void)is;(void)rc; }
PLUGINS_EXPORTDLL void ts3plugin_onFileListFinishedEvent(uint64 s, uint64 c, const char *p)                         { (void)s;(void)c;(void)p; }
PLUGINS_EXPORTDLL void ts3plugin_onFileInfoEvent(uint64 s, uint64 c, const char *n, uint64 sz, uint64 dt)           { (void)s;(void)c;(void)n;(void)sz;(void)dt; }
PLUGINS_EXPORTDLL void ts3plugin_onServerGroupListEvent(uint64 s, uint64 sg, const char *n, int t, int ic, int sd)  { (void)s;(void)sg;(void)n;(void)t;(void)ic;(void)sd; }
PLUGINS_EXPORTDLL void ts3plugin_onServerGroupListFinishedEvent(uint64 s)                                            { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onServerGroupByClientIDEvent(uint64 s, const char *n, uint64 sgl, uint64 cdb)      { (void)s;(void)n;(void)sgl;(void)cdb; }
PLUGINS_EXPORTDLL void ts3plugin_onServerGroupPermListEvent(uint64 s, uint64 sg, unsigned int pi, int pv, int pn, int ps) { (void)s;(void)sg;(void)pi;(void)pv;(void)pn;(void)ps; }
PLUGINS_EXPORTDLL void ts3plugin_onServerGroupPermListFinishedEvent(uint64 s, uint64 sg)                             { (void)s;(void)sg; }
PLUGINS_EXPORTDLL void ts3plugin_onServerGroupClientListEvent(uint64 s, uint64 sg, uint64 cdb, const char *cni, const char *cui) { (void)s;(void)sg;(void)cdb;(void)cni;(void)cui; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelGroupListEvent(uint64 s, uint64 cg, const char *n, int t, int ic, int sd) { (void)s;(void)cg;(void)n;(void)t;(void)ic;(void)sd; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelGroupListFinishedEvent(uint64 s)                                           { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelGroupPermListEvent(uint64 s, uint64 cg, unsigned int pi, int pv, int pn, int ps) { (void)s;(void)cg;(void)pi;(void)pv;(void)pn;(void)ps; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelGroupPermListFinishedEvent(uint64 s, uint64 cg)                            { (void)s;(void)cg; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelPermListEvent(uint64 s, uint64 c, unsigned int pi, int pv, int pn, int ps) { (void)s;(void)c;(void)pi;(void)pv;(void)pn;(void)ps; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelPermListFinishedEvent(uint64 s, uint64 c)                                  { (void)s;(void)c; }
PLUGINS_EXPORTDLL void ts3plugin_onClientPermListEvent(uint64 s, uint64 cdb, unsigned int pi, int pv, int pn, int ps) { (void)s;(void)cdb;(void)pi;(void)pv;(void)pn;(void)ps; }
PLUGINS_EXPORTDLL void ts3plugin_onClientPermListFinishedEvent(uint64 s, uint64 cdb)                                 { (void)s;(void)cdb; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelClientPermListEvent(uint64 s, uint64 c, uint64 cdb, unsigned int pi, int pv, int pn, int ps) { (void)s;(void)c;(void)cdb;(void)pi;(void)pv;(void)pn;(void)ps; }
PLUGINS_EXPORTDLL void ts3plugin_onChannelClientPermListFinishedEvent(uint64 s, uint64 c, uint64 cdb)               { (void)s;(void)c;(void)cdb; }
PLUGINS_EXPORTDLL void ts3plugin_onClientChannelGroupChangedEvent(uint64 s, uint64 cg, uint64 c, anyID ci, anyID ii, const char *in, const char *iu) { (void)s;(void)cg;(void)c;(void)ci;(void)ii;(void)in;(void)iu; }
PLUGINS_EXPORTDLL int  ts3plugin_onServerPermissionErrorEvent(uint64 s, const char *m, unsigned int e, const char *r, unsigned int fp) { (void)s;(void)m;(void)e;(void)r;(void)fp; return 0; }
PLUGINS_EXPORTDLL void ts3plugin_onPermissionListGroupEndIDEvent(uint64 s, unsigned int g)                           { (void)s;(void)g; }
PLUGINS_EXPORTDLL void ts3plugin_onPermissionListEvent(uint64 s, unsigned int pi, const char *pn, const char *pd)   { (void)s;(void)pi;(void)pn;(void)pd; }
PLUGINS_EXPORTDLL void ts3plugin_onPermissionListFinishedEvent(uint64 s)                                             { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onPermissionOverviewEvent(uint64 s, uint64 cdb, uint64 c, int ot, uint64 oi1, uint64 oi2, unsigned int pi, int pv, int pn, int ps) { (void)s;(void)cdb;(void)c;(void)ot;(void)oi1;(void)oi2;(void)pi;(void)pv;(void)pn;(void)ps; }
PLUGINS_EXPORTDLL void ts3plugin_onPermissionOverviewFinishedEvent(uint64 s)                                         { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onServerGroupClientAddedEvent(uint64 s, anyID ci, const char *cn, const char *cu, uint64 sg, anyID ii, const char *in, const char *iu) { (void)s;(void)ci;(void)cn;(void)cu;(void)sg;(void)ii;(void)in;(void)iu; }
PLUGINS_EXPORTDLL void ts3plugin_onServerGroupClientDeletedEvent(uint64 s, anyID ci, const char *cn, const char *cu, uint64 sg, anyID ii, const char *in, const char *iu) { (void)s;(void)ci;(void)cn;(void)cu;(void)sg;(void)ii;(void)in;(void)iu; }
PLUGINS_EXPORTDLL void ts3plugin_onClientNeededPermissionsEvent(uint64 s, unsigned int pi, int pv)                   { (void)s;(void)pi;(void)pv; }
PLUGINS_EXPORTDLL void ts3plugin_onClientNeededPermissionsFinishedEvent(uint64 s)                                    { (void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onFileTransferStatusEvent(anyID t, unsigned int st, const char *sm, uint64 rs, uint64 s) { (void)t;(void)st;(void)sm;(void)rs;(void)s; }
PLUGINS_EXPORTDLL void ts3plugin_onClientChatClosedEvent(uint64 s, anyID c, const char *cu)                         { (void)s;(void)c;(void)cu; }
PLUGINS_EXPORTDLL void ts3plugin_onClientChatComposingEvent(uint64 s, anyID c, const char *cu)                      { (void)s;(void)c;(void)cu; }
PLUGINS_EXPORTDLL void ts3plugin_onServerLogEvent(uint64 s, const char *m)                                          { (void)s;(void)m; }
PLUGINS_EXPORTDLL void ts3plugin_onServerLogFinishedEvent(uint64 s, uint64 lp, uint64 fs)                           { (void)s;(void)lp;(void)fs; }
PLUGINS_EXPORTDLL void ts3plugin_onMessageListEvent(uint64 s, uint64 mid, const char *fcu, const char *sub, uint64 ts, int fr) { (void)s;(void)mid;(void)fcu;(void)sub;(void)ts;(void)fr; }
PLUGINS_EXPORTDLL void ts3plugin_onMessageGetEvent(uint64 s, uint64 mid, const char *fcu, const char *sub, const char *msg, uint64 ts) { (void)s;(void)mid;(void)fcu;(void)sub;(void)msg;(void)ts; }
PLUGINS_EXPORTDLL void ts3plugin_onClientDBIDfromUIDEvent(uint64 s, const char *uid, uint64 cdb)                    { (void)s;(void)uid;(void)cdb; }
PLUGINS_EXPORTDLL void ts3plugin_onClientNamefromUIDEvent(uint64 s, const char *uid, uint64 cdb, const char *n)     { (void)s;(void)uid;(void)cdb;(void)n; }
PLUGINS_EXPORTDLL void ts3plugin_onClientNamefromDBIDEvent(uint64 s, const char *uid, uint64 cdb, const char *n)    { (void)s;(void)uid;(void)cdb;(void)n; }
PLUGINS_EXPORTDLL void ts3plugin_onComplainListEvent(uint64 s, uint64 tcdb, const char *tcn, uint64 fcdb, const char *fcn, const char *r, uint64 ts) { (void)s;(void)tcdb;(void)tcn;(void)fcdb;(void)fcn;(void)r;(void)ts; }
PLUGINS_EXPORTDLL void ts3plugin_onBanListEvent(uint64 s, uint64 bid, const char *ip, const char *n, const char *uid, uint64 ct, uint64 dt, const char *in, uint64 ic, const char *iu, const char *r, int ne, const char *ln) { (void)s;(void)bid;(void)ip;(void)n;(void)uid;(void)ct;(void)dt;(void)in;(void)ic;(void)iu;(void)r;(void)ne;(void)ln; }
PLUGINS_EXPORTDLL void ts3plugin_onClientServerQueryLoginPasswordEvent(uint64 s, const char *lp)                    { (void)s;(void)lp; }
PLUGINS_EXPORTDLL void ts3plugin_onPluginCommandEvent(uint64 s, const char *pn, const char *pc)                     { (void)s;(void)pn;(void)pc; }
PLUGINS_EXPORTDLL void ts3plugin_onPluginCommandEvent_v23(uint64 s, const char *pn, const char *pc, anyID ic, const char *in, const char *iu) { (void)s;(void)pn;(void)pc;(void)ic;(void)in;(void)iu; }
PLUGINS_EXPORTDLL void ts3plugin_onIncomingClientQueryEvent(uint64 s, const char *ct)                               { (void)s;(void)ct; }
PLUGINS_EXPORTDLL void ts3plugin_onServerTemporaryPasswordListEvent(uint64 s, const char *cn, const char *cui, const char *d, const char *pw, uint64 ts, uint64 te, uint64 tc, const char *tcp) { (void)s;(void)cn;(void)cui;(void)d;(void)pw;(void)ts;(void)te;(void)tc;(void)tcp; }
PLUGINS_EXPORTDLL void ts3plugin_onAvatarUpdated(uint64 s, anyID c, const char *ap)                                 { (void)s;(void)c;(void)ap; }
PLUGINS_EXPORTDLL void ts3plugin_onMenuItemEvent(uint64 s, enum PluginMenuType t, int i, uint64 si)                 { (void)s;(void)t;(void)i;(void)si; }
PLUGINS_EXPORTDLL void ts3plugin_onHotkeyEvent(const char *k)                                                       { (void)k; }
PLUGINS_EXPORTDLL void ts3plugin_onHotkeyRecordedEvent(const char *k, const char *r)                                { (void)k;(void)r; }
PLUGINS_EXPORTDLL void ts3plugin_onClientDisplayNameChanged(uint64 s, anyID c, const char *dn, const char *uid)     { (void)s;(void)c;(void)dn;(void)uid; }
