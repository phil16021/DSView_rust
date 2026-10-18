/*
 * A fake libusb for the oracle: the libusb_* functions libsigrok4DSL uses,
 * backed by the simulated DSL device of src/usbsim.rs (the same device the
 * Rust drivers get through dsview-usb's SimBackend). The oracle links this
 * instead of libusb-1.0, so the C DSCope/DSLogic drivers can be compared
 * with the Rust ones without hardware.
 *
 * Asynchronous transfers complete in ticks: each libusb_handle_events*()
 * call from a driver completes, oldest first, every queued transfer the
 * device has data for; a transfer submitted from a callback waits for the
 * next call. The libsigrok session waits in g_poll(); session.c is built
 * with g_poll mapped to oracle_poll(), which reports the event fd readable
 * when a tick would complete something and otherwise lets the virtual clock
 * run for the whole timeout. Hotplug never fires, and the hotplug thread's
 * event pumping completes nothing (real libusb lets that thread complete
 * the drivers' transfers too, a race the comparison leaves out).
 */

#include <glib.h>
#include <libusb.h>
#include <string.h>
#include <poll.h>
#include <unistd.h>

#include "oracle.h"

int oracle_sim_present(void);
void oracle_sim_info(uint16_t *vid, uint16_t *pid, int *speed, uint8_t *bus, uint8_t *address);
int oracle_sim_string(uint8_t idx, char *buf, int len);
void oracle_sim_open(void);
void oracle_sim_close(void);
int oracle_sim_control(uint8_t request_type, uint8_t request, uint16_t value, uint16_t index,
                       unsigned char *data, uint16_t len);
int oracle_sim_bulk_out(uint8_t ep, const unsigned char *data, int len);
void oracle_sim_submit(uint64_t id, int len);
void oracle_sim_cancel(uint64_t id);
int oracle_sim_can_complete(void);
void oracle_sim_tick(void);
int oracle_sim_take(uint64_t *id, int *status, const unsigned char **data, int *len);
void oracle_usleep(unsigned long us);

struct libusb_context {
    int unused;
};
struct libusb_device {
    int unused;
};
struct libusb_device_handle {
    struct libusb_device *dev;
};

static struct libusb_context g_ctx;
static struct libusb_device g_dev;
static int g_event_fd = -1;

/* The fd the drivers poll: a pipe nobody writes to. */
static int event_fd(void)
{
    if (g_event_fd < 0) {
        int p[2];
        if (pipe(p) == 0)
            g_event_fd = p[0];
    }
    return g_event_fd;
}

int LIBUSB_CALL libusb_init(libusb_context **ctx)
{
    if (ctx)
        *ctx = &g_ctx;
    return 0;
}

void LIBUSB_CALL libusb_exit(libusb_context *ctx)
{
    (void)ctx;
}

ssize_t LIBUSB_CALL libusb_get_device_list(libusb_context *ctx, libusb_device ***list)
{
    (void)ctx;
    libusb_device **l = g_malloc0(2 * sizeof(*l));
    ssize_t n = 0;
    if (oracle_sim_present())
        l[n++] = &g_dev;
    *list = l;
    return n;
}

void LIBUSB_CALL libusb_free_device_list(libusb_device **list, int unref_devices)
{
    (void)unref_devices;
    g_free(list);
}

libusb_device *LIBUSB_CALL libusb_ref_device(libusb_device *dev)
{
    return dev;
}

void LIBUSB_CALL libusb_unref_device(libusb_device *dev)
{
    (void)dev;
}

int LIBUSB_CALL libusb_get_device_descriptor(libusb_device *dev, struct libusb_device_descriptor *desc)
{
    uint16_t vid = 0, pid = 0;
    int speed = 0;
    uint8_t bus = 0, address = 0;
    (void)dev;
    oracle_sim_info(&vid, &pid, &speed, &bus, &address);
    memset(desc, 0, sizeof(*desc));
    desc->bLength = LIBUSB_DT_DEVICE_SIZE;
    desc->bDescriptorType = LIBUSB_DT_DEVICE;
    desc->bcdUSB = 0x0200;
    desc->bMaxPacketSize0 = 64;
    desc->idVendor = vid;
    desc->idProduct = pid;
    desc->iManufacturer = 1;
    desc->iProduct = 2;
    desc->bNumConfigurations = 1;
    return 0;
}

uint8_t LIBUSB_CALL libusb_get_bus_number(libusb_device *dev)
{
    uint16_t vid, pid;
    int speed;
    uint8_t bus = 0, address = 0;
    (void)dev;
    oracle_sim_info(&vid, &pid, &speed, &bus, &address);
    return bus;
}

uint8_t LIBUSB_CALL libusb_get_device_address(libusb_device *dev)
{
    uint16_t vid, pid;
    int speed;
    uint8_t bus = 0, address = 0;
    (void)dev;
    oracle_sim_info(&vid, &pid, &speed, &bus, &address);
    return address;
}

int LIBUSB_CALL libusb_get_device_speed(libusb_device *dev)
{
    uint16_t vid, pid;
    int speed = 0;
    uint8_t bus, address;
    (void)dev;
    oracle_sim_info(&vid, &pid, &speed, &bus, &address);
    return speed;
}

int LIBUSB_CALL libusb_get_config_descriptor(libusb_device *dev, uint8_t config_index,
                                             struct libusb_config_descriptor **config)
{
    (void)dev;
    (void)config_index;
    *config = NULL;
    return LIBUSB_ERROR_NOT_FOUND;
}

void LIBUSB_CALL libusb_free_config_descriptor(struct libusb_config_descriptor *config)
{
    (void)config;
}

int LIBUSB_CALL libusb_open(libusb_device *dev, libusb_device_handle **dev_handle)
{
    if (!oracle_sim_present())
        return LIBUSB_ERROR_NO_DEVICE;
    *dev_handle = g_malloc0(sizeof(**dev_handle));
    (*dev_handle)->dev = dev;
    oracle_sim_open();
    return 0;
}

void LIBUSB_CALL libusb_close(libusb_device_handle *dev_handle)
{
    if (dev_handle == NULL)
        return;
    oracle_sim_close();
    g_free(dev_handle);
}

int LIBUSB_CALL libusb_claim_interface(libusb_device_handle *dev_handle, int interface_number)
{
    (void)dev_handle;
    (void)interface_number;
    return 0;
}

int LIBUSB_CALL libusb_release_interface(libusb_device_handle *dev_handle, int interface_number)
{
    (void)dev_handle;
    (void)interface_number;
    return 0;
}

int LIBUSB_CALL libusb_set_configuration(libusb_device_handle *dev_handle, int configuration)
{
    (void)dev_handle;
    (void)configuration;
    return 0;
}

int LIBUSB_CALL libusb_kernel_driver_active(libusb_device_handle *dev_handle, int interface_number)
{
    (void)dev_handle;
    (void)interface_number;
    return 0;
}

int LIBUSB_CALL libusb_detach_kernel_driver(libusb_device_handle *dev_handle, int interface_number)
{
    (void)dev_handle;
    (void)interface_number;
    return 0;
}

int LIBUSB_CALL libusb_get_string_descriptor_ascii(libusb_device_handle *dev_handle, uint8_t desc_index,
                                                   unsigned char *data, int length)
{
    (void)dev_handle;
    return oracle_sim_string(desc_index, (char *)data, length);
}

int LIBUSB_CALL libusb_control_transfer(libusb_device_handle *dev_handle, uint8_t request_type,
                                        uint8_t bRequest, uint16_t wValue, uint16_t wIndex,
                                        unsigned char *data, uint16_t wLength, unsigned int timeout)
{
    (void)dev_handle;
    (void)timeout;
    return oracle_sim_control(request_type, bRequest, wValue, wIndex, data, wLength);
}

int LIBUSB_CALL libusb_bulk_transfer(libusb_device_handle *dev_handle, unsigned char endpoint,
                                     unsigned char *data, int length, int *actual_length,
                                     unsigned int timeout)
{
    int n;
    (void)dev_handle;
    (void)timeout;
    if (endpoint & LIBUSB_ENDPOINT_IN)
        return LIBUSB_ERROR_NOT_SUPPORTED; /* the DSL drivers read asynchronously */
    n = oracle_sim_bulk_out(endpoint, data, length);
    if (n < 0)
        return n;
    if (actual_length)
        *actual_length = n;
    return 0;
}

struct libusb_transfer *LIBUSB_CALL libusb_alloc_transfer(int iso_packets)
{
    (void)iso_packets;
    return g_malloc0(sizeof(struct libusb_transfer));
}

void LIBUSB_CALL libusb_free_transfer(struct libusb_transfer *transfer)
{
    if (transfer && (transfer->flags & LIBUSB_TRANSFER_FREE_BUFFER))
        g_free(transfer->buffer);
    g_free(transfer);
}

int LIBUSB_CALL libusb_submit_transfer(struct libusb_transfer *transfer)
{
    oracle_sim_submit((uint64_t)(uintptr_t)transfer, transfer->length);
    return 0;
}

int LIBUSB_CALL libusb_cancel_transfer(struct libusb_transfer *transfer)
{
    oracle_sim_cancel((uint64_t)(uintptr_t)transfer);
    return 0;
}

/* One tick: completes what the device has data for, calls the callbacks. */
static void run_tick(void)
{
    uint64_t id;
    int status, len;
    const unsigned char *data;

    oracle_sim_tick();
    while (oracle_sim_take(&id, &status, &data, &len)) {
        struct libusb_transfer *t = (struct libusb_transfer *)(uintptr_t)id;
        if (len > t->length)
            len = t->length;
        if (len > 0)
            memcpy(t->buffer, data, len);
        t->actual_length = len;
        t->status = status ? LIBUSB_TRANSFER_CANCELLED : LIBUSB_TRANSFER_COMPLETED;
        if (t->callback)
            t->callback(t);
    }
}

int LIBUSB_CALL libusb_handle_events_timeout_completed(libusb_context *ctx, struct timeval *tv,
                                                       int *completed)
{
    (void)ctx;
    (void)tv;
    (void)completed;
    run_tick();
    return 0;
}

/* The hotplug thread's pump: nothing to do. */
int LIBUSB_CALL libusb_handle_events_timeout(libusb_context *ctx, struct timeval *tv)
{
    (void)ctx;
    (void)tv;
    return 0;
}

int LIBUSB_CALL libusb_handle_events(libusb_context *ctx)
{
    (void)ctx;
    return 0;
}

void LIBUSB_CALL libusb_interrupt_event_handler(libusb_context *ctx)
{
    (void)ctx;
}

const struct libusb_pollfd **LIBUSB_CALL libusb_get_pollfds(libusb_context *ctx)
{
    static struct libusb_pollfd pfd;
    const struct libusb_pollfd **l = g_malloc0(2 * sizeof(*l));
    (void)ctx;
    pfd.fd = event_fd();
    pfd.events = POLLIN;
    l[0] = &pfd;
    return l;
}

void LIBUSB_CALL libusb_free_pollfds(const struct libusb_pollfd **pollfds)
{
    g_free((void *)pollfds);
}

int LIBUSB_CALL libusb_hotplug_register_callback(libusb_context *ctx, int events, int flags,
                                                 int vendor_id, int product_id, int dev_class,
                                                 libusb_hotplug_callback_fn cb_fn, void *user_data,
                                                 libusb_hotplug_callback_handle *callback_handle)
{
    (void)ctx;
    (void)events;
    (void)flags;
    (void)vendor_id;
    (void)product_id;
    (void)dev_class;
    (void)cb_fn;
    (void)user_data;
    if (callback_handle)
        *callback_handle = 1;
    return 0;
}

void LIBUSB_CALL libusb_hotplug_deregister_callback(libusb_context *ctx,
                                                    libusb_hotplug_callback_handle callback_handle)
{
    (void)ctx;
    (void)callback_handle;
}

const char *LIBUSB_CALL libusb_error_name(int errcode)
{
    switch (errcode) {
    case 0: return "LIBUSB_SUCCESS";
    case LIBUSB_ERROR_IO: return "LIBUSB_ERROR_IO";
    case LIBUSB_ERROR_INVALID_PARAM: return "LIBUSB_ERROR_INVALID_PARAM";
    case LIBUSB_ERROR_ACCESS: return "LIBUSB_ERROR_ACCESS";
    case LIBUSB_ERROR_NO_DEVICE: return "LIBUSB_ERROR_NO_DEVICE";
    case LIBUSB_ERROR_NOT_FOUND: return "LIBUSB_ERROR_NOT_FOUND";
    case LIBUSB_ERROR_BUSY: return "LIBUSB_ERROR_BUSY";
    case LIBUSB_ERROR_TIMEOUT: return "LIBUSB_ERROR_TIMEOUT";
    case LIBUSB_ERROR_PIPE: return "LIBUSB_ERROR_PIPE";
    case LIBUSB_ERROR_NOT_SUPPORTED: return "LIBUSB_ERROR_NOT_SUPPORTED";
    default: return "LIBUSB_ERROR_OTHER";
    }
}

/* g_poll() for session.c: the event fd is readable when a tick would
 * complete a transfer; otherwise the virtual clock runs the timeout. Polls
 * without the event fd go to the real g_poll(). */
gint oracle_poll(GPollFD *fds, guint nfds, gint timeout)
{
    guint i;
    int ours = -1;

    for (i = 0; i < nfds; i++) {
        fds[i].revents = 0;
        if (g_event_fd >= 0 && fds[i].fd == g_event_fd)
            ours = (int)i;
    }
    if (ours < 0)
        return g_poll(fds, nfds, timeout);
    if (oracle_sim_can_complete()) {
        fds[ours].revents = G_IO_IN;
        return 1;
    }
    if (timeout > 0)
        oracle_usleep((unsigned long)timeout * 1000);
    return 0;
}
