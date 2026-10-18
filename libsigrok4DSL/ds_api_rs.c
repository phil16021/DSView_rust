/*
 * The ds_* API on the Rust acquisition library (crates/dsview-session via
 * dsv_lib_* in dsview_ffi.h), used instead of lib_main.c's when
 * DSVIEW_IMPL_ACQ=rust (or DSVIEW_IMPL=rust): lib_main.c's entry points hand
 * over to the rs_ds_* functions here. Transitional: removed with
 * libsigrok4DSL.
 *
 * The library keeps the devices (the Rust drivers of every kind), the
 * active device, hotplug and the capture thread. The GUI still sees each
 * device as a struct sr_dev_inst with a GSList of sr_channel: one copy per
 * device, made by rs_driver.c and kept here by handle. The library calls
 * back to bring a copy and its device in step (sync_in before it uses the
 * device, sync_out after), to forward packets and events, and when a device
 * is gone. Those callbacks run on the library's threads with its lock held,
 * so they only touch the copies, never the library.
 */

#ifdef DSVIEW_RUST_CORE

#include "libsigrok-internal.h"
#include "log.h"
#include <glib.h>
#include <string.h>
#include "dsview_ffi.h"

#undef LOG_PREFIX
#define LOG_PREFIX "lib_main: "

extern char DS_RES_PATH[500];
extern char DS_USR_PATH[500];

SR_PRIV struct sr_dev_inst *rs_mirror_new(struct dsv_device *h, int driver, uint8_t bus,
                                          uint8_t address, uint64_t handle);
SR_PRIV void rs_mirror_put(const struct sr_dev_inst *sdi);
SR_PRIV void rs_mirror_get(struct sr_dev_inst *sdi, int full);
SR_PRIV void rs_mirror_set_trigger(const struct sr_dev_inst *sdi);
SR_PRIV void rs_mirror_set_location(struct sr_dev_inst *sdi, uint8_t bus, uint8_t address);
SR_PRIV void rs_mirror_free(struct sr_dev_inst *sdi);
SR_PRIV void rs_mirror_emit(void *ctx, const struct dsv_packet *p);
SR_PRIV void rs_mirror_log(int32_t level, const char *msg);
SR_PRIV int sr_dslogic_option_value_to_code(const struct sr_dev_inst *sdi, int config_id, const char *value);
SR_PRIV int sr_dscope_option_value_to_code(const struct sr_dev_inst *sdi, int config_id, const char *value);

static GMutex mirrors_lock;
static GHashTable *mirrors; /* handle -> struct sr_dev_inst* */
static dslib_event_callback_t event_cb;
static ds_datafeed_callback_t datafeed_cb;

static struct sr_dev_inst *mirror(uint64_t handle)
{
    struct sr_dev_inst *sdi = NULL;
    g_mutex_lock(&mirrors_lock);
    if (mirrors != NULL)
        sdi = g_hash_table_lookup(mirrors, &handle);
    g_mutex_unlock(&mirrors_lock);
    return sdi;
}

static guint handle_hash(gconstpointer k)
{
    uint64_t h = *(const uint64_t *)k;
    return (guint)(h ^ (h >> 32));
}

static gboolean handle_equal(gconstpointer a, gconstpointer b)
{
    return *(const uint64_t *)a == *(const uint64_t *)b;
}

/* Makes the copy of a listed device if it has none yet (not from the
 * library's callbacks: it asks the library). */
static struct sr_dev_inst *ensure_mirror(const struct dsv_lib_entry *e)
{
    struct sr_dev_inst *sdi = mirror(e->handle);
    if (sdi != NULL) {
        rs_mirror_set_location(sdi, e->bus, e->address);
        return sdi;
    }
    sdi = rs_mirror_new(dsv_lib_device(e->handle), e->driver, e->bus, e->address, e->handle);
    if (sdi == NULL)
        return NULL;
    g_mutex_lock(&mirrors_lock);
    if (mirrors == NULL)
        mirrors = g_hash_table_new_full(handle_hash, handle_equal, g_free, NULL);
    {
        uint64_t *k = g_new(uint64_t, 1);
        *k = e->handle;
        g_hash_table_insert(mirrors, k, sdi);
    }
    g_mutex_unlock(&mirrors_lock);
    return sdi;
}

/* Copies for every listed device; returns the entries (g_free). */
static struct dsv_lib_entry *list_entries(uint32_t *count)
{
    uint32_t n = dsv_lib_devices(NULL, 0), got;
    struct dsv_lib_entry *list = g_new0(struct dsv_lib_entry, n + 1);
    got = dsv_lib_devices(list, n);
    if (got < n)
        n = got;
    for (uint32_t i = 0; i < n; i++)
        ensure_mirror(&list[i]);
    *count = n;
    return list;
}

static struct sr_dev_inst *active_sdi(void)
{
    struct dsv_lib_entry e;
    if (!dsv_lib_active(&e))
        return NULL;
    return ensure_mirror(&e);
}

/* ------------------------------------------------------ host callbacks */

static void host_event(void *user, int32_t ev)
{
    (void)user;
    if (event_cb != NULL)
        event_cb(ev);
}

static void *host_packet_ctx(void *user, uint64_t handle)
{
    (void)user;
    return mirror(handle);
}

static void host_sync_in(void *user, uint64_t handle)
{
    struct sr_dev_inst *sdi = mirror(handle);
    (void)user;
    if (sdi != NULL)
        rs_mirror_put(sdi);
}

static void host_sync_out(void *user, uint64_t handle, int32_t full)
{
    struct sr_dev_inst *sdi = mirror(handle);
    (void)user;
    if (sdi != NULL)
        rs_mirror_get(sdi, full);
}

static void host_before_start(void *user, uint64_t handle)
{
    struct sr_dev_inst *sdi = mirror(handle);
    (void)user;
    if (sdi != NULL)
        rs_mirror_set_trigger(sdi);
}

static void host_removed(void *user, uint64_t handle)
{
    struct sr_dev_inst *sdi = NULL;
    (void)user;
    g_mutex_lock(&mirrors_lock);
    if (mirrors != NULL) {
        sdi = g_hash_table_lookup(mirrors, &handle);
        g_hash_table_remove(mirrors, &handle);
    }
    g_mutex_unlock(&mirrors_lock);
    if (sdi != NULL)
        rs_mirror_free(sdi);
}

static void host_emit(void *ctx, const struct dsv_packet *p)
{
    if (ctx != NULL)
        rs_mirror_emit(ctx, p);
}

/* ------------------------------------------------------------- the API */

SR_PRIV int rs_ds_lib_init(void)
{
    static const struct dsv_lib_host host = {
        .user = NULL,
        .event = host_event,
        .packet_ctx = host_packet_ctx,
        .emit = host_emit,
        .sync_in = host_sync_in,
        .sync_out = host_sync_out,
        .before_start = host_before_start,
        .removed = host_removed,
    };
    int ret;
    uint32_t n;

    sr_log_init();
    sr_info("Init %s (Rust).", SR_LIB_NAME);
    ds_trigger_init();
    dsv_drv_set_log(rs_mirror_log);
    dsv_drv_set_user_dir(DS_USR_PATH);
    dsv_drv_set_res_dir(DS_RES_PATH);
    ret = dsv_lib_init(&host);
    if (ret != SR_OK)
        return ret;
    g_free(list_entries(&n));
    return SR_OK;
}

SR_PRIV int rs_ds_lib_exit(void)
{
    dsv_lib_exit();
    /* copies the library did not hand back (it frees through `removed`) */
    g_mutex_lock(&mirrors_lock);
    if (mirrors != NULL) {
        GHashTableIter it;
        gpointer k, v;
        g_hash_table_iter_init(&it, mirrors);
        while (g_hash_table_iter_next(&it, &k, &v))
            rs_mirror_free(v);
        g_hash_table_destroy(mirrors);
        mirrors = NULL;
    }
    g_mutex_unlock(&mirrors_lock);
    ds_trigger_destroy();
    sr_log_uninit();
    return SR_OK;
}

SR_PRIV void rs_ds_set_event_callback(dslib_event_callback_t cb)
{
    event_cb = cb;
}

SR_PRIV void rs_ds_set_datafeed_callback(ds_datafeed_callback_t cb)
{
    datafeed_cb = cb;
}

SR_PRIV void rs_ds_set_dirs(void)
{
    dsv_drv_set_user_dir(DS_USR_PATH);
    dsv_drv_set_res_dir(DS_RES_PATH);
}

SR_PRIV int rs_ds_get_device_list(struct ds_device_base_info **out_list, int *out_count)
{
    struct dsv_lib_entry *list;
    struct ds_device_base_info *p;
    uint32_t n;

    if (out_list == NULL)
        return SR_ERR_ARG;
    *out_list = NULL;
    list = list_entries(&n);
    if (n == 0) {
        g_free(list);
        return SR_OK;
    }
    p = g_try_malloc0(sizeof(*p) * (n + 1));
    if (p == NULL) {
        g_free(list);
        return SR_ERR_MALLOC;
    }
    for (uint32_t i = 0; i < n; i++) {
        p[i].handle = list[i].handle;
        strncpy(p[i].name, list[i].name, sizeof(p[i].name) - 1);
        strncpy(p[i].unique_id, list[i].unique_id, sizeof(p[i].unique_id) - 1);
    }
    if (out_count)
        *out_count = (int)n;
    g_free(list);
    *out_list = p;
    return SR_OK;
}

SR_PRIV int rs_ds_active_device(ds_device_handle handle)
{
    uint32_t n;
    g_free(list_entries(&n));
    return dsv_lib_activate(handle);
}

SR_PRIV int rs_ds_active_device_by_index(int index)
{
    uint32_t n;
    g_free(list_entries(&n));
    return dsv_lib_activate_index(index);
}

SR_PRIV int rs_ds_get_actived_device_index(void)
{
    return dsv_lib_active_index();
}

SR_PRIV int rs_ds_have_actived_device(void)
{
    return dsv_lib_active(NULL);
}

SR_PRIV int rs_ds_device_from_file(const char *file_path)
{
    uint32_t n;
    int ret = dsv_lib_device_from_file(file_path);
    if (ret == SR_OK)
        g_free(list_entries(&n));
    return ret;
}

SR_PRIV int rs_ds_remove_device(ds_device_handle handle)
{
    return dsv_lib_remove(handle);
}

SR_PRIV const GSList *rs_ds_get_actived_device_mode_list(void)
{
    struct sr_dev_inst *sdi = active_sdi();
    if (sdi == NULL) {
        sr_err("Have no actived device.");
        return NULL;
    }
    return sdi->driver->dev_mode_list(sdi);
}

SR_PRIV int rs_ds_get_actived_device_info(struct ds_device_full_info *p)
{
    struct dsv_lib_entry e;

    if (p == NULL)
        return SR_ERR_ARG;
    p->handle = NULL_HANDLE;
    p->name[0] = '\0';
    p->path[0] = '\0';
    p->driver_name[0] = '\0';
    p->dev_type = DEV_TYPE_UNKOWN;
    p->di = NULL;
    p->actived_times = 0;
    if (!dsv_lib_active(&e))
        return SR_ERR_CALL_STATUS;
    p->handle = e.handle;
    p->dev_type = e.dev_type;
    p->actived_times = e.actived_times;
    p->di = ensure_mirror(&e);
    strncpy(p->name, e.name, sizeof(p->name) - 1);
    if (p->di != NULL && p->di->driver && p->di->driver->name)
        strncpy(p->driver_name, p->di->driver->name, sizeof(p->driver_name) - 1);
    if (e.dev_type == DEV_TYPE_FILELOG || e.dev_type == DEV_TYPE_DEMO)
        strncpy(p->path, e.path, sizeof(p->path) - 1);
    return SR_OK;
}

SR_PRIV int rs_ds_get_actived_device_mode(void)
{
    struct sr_dev_inst *sdi = active_sdi();
    return sdi != NULL ? sdi->mode : UNKNOWN_DSL_MODE;
}

SR_PRIV int rs_ds_start_collect(void)
{
    struct sr_dev_inst *sdi = active_sdi();
    (void)sdi; /* the library copies the channels in itself */
    return dsv_lib_start_collect(datafeed_cb != NULL);
}

SR_PRIV int rs_ds_stop_collect(void)
{
    return dsv_lib_stop_collect();
}

SR_PRIV int rs_ds_is_collecting(void)
{
    return dsv_lib_is_collecting();
}

SR_PRIV int rs_ds_release_actived_device(void)
{
    return dsv_lib_release_active();
}

SR_PRIV int rs_ds_get_last_error(void)
{
    return dsv_lib_last_error();
}

SR_PRIV void rs_ds_set_last_error(int error)
{
    dsv_lib_set_last_error(error);
}

SR_PRIV int rs_ds_reload_device_list(void)
{
    uint32_t n;
    dsv_lib_reload();
    g_free(list_entries(&n));
    return SR_OK;
}

SR_PRIV int rs_ds_close_all_device(void)
{
    return dsv_lib_close_all();
}

SR_PRIV void rs_ds_session_stop(void)
{
    dsv_lib_request_stop();
}

/* sr_dev_inst-level calls: on the active device's copy, as lib_main.c does
 * on its sr_dev_inst. The copy's driver is a Rust driver of rs_driver.c, so
 * these go to the device (with the channels copied in and out there). */

SR_PRIV struct sr_dev_inst *rs_ds_active_sdi(void)
{
    return active_sdi();
}

SR_PRIV int rs_ds_option_value_to_code(int work_mode, int config_id, const char *value)
{
    struct sr_dev_inst *sdi = active_sdi();
    if (work_mode == LOGIC)
        return sr_dslogic_option_value_to_code(sdi, config_id, value);
    return sr_dscope_option_value_to_code(sdi, config_id, value);
}

#endif /* DSVIEW_RUST_CORE */
