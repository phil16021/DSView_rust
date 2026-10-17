/*
 * .dsl oracle: opens a capture file through libsigrok4DSL's virtual-session
 * driver, exactly like SigSession::set_file + start_capture, and forwards the
 * datafeed packets so the Rust reader can be compared against them.
 */

#include <glib.h>
#include <string.h>
#include "libsigrok.h"

#include "oracle.h"

static GMutex g_lock;
static GCond g_done_cond;
static gboolean g_done;
static oracle_packet_cb g_cb;
static void *g_user;

static void on_datafeed(const struct sr_dev_inst *sdi, const struct sr_datafeed_packet *packet)
{
    (void)sdi;

    switch (packet->type) {
    case SR_DF_LOGIC: {
        const struct sr_datafeed_logic *l = packet->payload;
        g_cb(g_user, ORACLE_PKT_LOGIC, l->format == LA_CROSS_DATA ? 0 : 1, l->data, l->length, 0);
        break;
    }
    case SR_DF_DSO: {
        const struct sr_datafeed_dso *d = packet->payload;
        g_cb(g_user, ORACLE_PKT_DSO, 0, d->data, 0, d->num_samples);
        break;
    }
    case SR_DF_ANALOG: {
        const struct sr_datafeed_analog *a = packet->payload;
        g_cb(g_user, ORACLE_PKT_ANALOG, 0, a->data, 0, a->num_samples);
        break;
    }
    case SR_DF_END:
        g_cb(g_user, ORACLE_PKT_END, 0, NULL, 0, 0);
        g_mutex_lock(&g_lock);
        g_done = TRUE;
        g_cond_signal(&g_done_cond);
        g_mutex_unlock(&g_lock);
        break;
    default:
        g_cb(g_user, ORACLE_PKT_OTHER, 0, NULL, 0, 0);
        break;
    }
}

static void on_event(int event)
{
    if (event == DS_EV_COLLECT_TASK_END || event == DS_EV_COLLECT_TASK_END_BY_ERROR ||
        event == DS_EV_COLLECT_TASK_END_BY_DETACHED) {
        g_mutex_lock(&g_lock);
        g_done = TRUE;
        g_cond_signal(&g_done_cond);
        g_mutex_unlock(&g_lock);
    }
}

static gboolean get_u64(int key, uint64_t *out)
{
    GVariant *v = NULL;
    if (ds_get_actived_device_config(NULL, NULL, key, &v) != SR_OK || v == NULL)
        return FALSE;
    *out = g_variant_get_uint64(v);
    g_variant_unref(v);
    return TRUE;
}

int oracle_dsl_run(const char *path, int log_level, struct oracle_dsl_info *info,
                   oracle_packet_cb cb, void *user)
{
    int ret = -1;

    memset(info, 0, sizeof(*info));
    g_cb = cb;
    g_user = user;
    g_done = FALSE;

    ds_log_level(log_level);
    ds_set_datafeed_callback(on_datafeed);
    ds_set_event_callback(on_event);
    if (ds_lib_init() != SR_OK)
        return -1;

    if (ds_device_from_file(path) != SR_OK)
        goto out;
    /* SigSession activates the file device it just appended (the last one). */
    if (ds_active_device_by_index(-1) != SR_OK)
        goto out;

    info->mode = ds_get_actived_device_mode();
    get_u64(SR_CONF_SAMPLERATE, &info->samplerate);
    get_u64(SR_CONF_LIMIT_SAMPLES, &info->limit_samples);

    for (GSList *l = ds_get_actived_device_channels(); l && info->n_channels < ORACLE_MAX_CHANNELS; l = l->next) {
        const struct sr_channel *ch = l->data;
        struct oracle_channel *o = &info->channels[info->n_channels++];
        o->index = ch->index;
        o->type = ch->type == SR_CHANNEL_LOGIC ? ORACLE_CH_LOGIC :
                  ch->type == SR_CHANNEL_DSO ? ORACLE_CH_DSO :
                  ch->type == SR_CHANNEL_ANALOG ? ORACLE_CH_ANALOG : ORACLE_CH_OTHER;
        o->enabled = ch->enabled;
        g_strlcpy(o->name, ch->name ? ch->name : "", sizeof(o->name));
    }

    if (ds_start_collect() != SR_OK)
        goto out;

    g_mutex_lock(&g_lock);
    gint64 deadline = g_get_monotonic_time() + 60 * G_TIME_SPAN_SECOND;
    while (!g_done) {
        if (!g_cond_wait_until(&g_done_cond, &g_lock, deadline))
            break;
    }
    gboolean finished = g_done;
    g_mutex_unlock(&g_lock);

    ds_stop_collect();
    ret = finished ? 0 : -2;

out:
    ds_release_actived_device();
    ds_lib_exit();
    return ret;
}
