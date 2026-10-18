import sigrokdecode as srd

class Decoder(srd.Decoder):
    api_version = 3
    id = 'pending_error'
    name = 'Pending error'
    longname = 'Pending wait() error'
    desc = 'Test decoder for errors left pending by wait() conditions.'
    license = 'gplv2+'
    inputs = ['logic']
    outputs = []
    tags = ['Util']
    channels = (
        {'id': 'data', 'name': 'Data', 'desc': 'Data line', 'idn': 'dec_pending_error_chan_data'},
    )
    annotations = (
        ('survived', 'wait() returned normally'),
    )

    def reset(self):
        pass

    def start(self):
        self.out_ann = self.register(srd.OUTPUT_ANN)

    def decode(self):
        self.wait({0: 'r'})
        # Changing the class invalidates its type-cache entries, so storing
        # self.samplenum inside the next wait() misses the cache.
        type(self).generation = getattr(type(self), 'generation', 0) + 1
        self.wait([{'skip': -1}, {0: 'f'}])
        self.put(self.samplenum, self.samplenum, self.out_ann, [0, ['survived']])
        while True:
            self.wait({0: 'e'})
