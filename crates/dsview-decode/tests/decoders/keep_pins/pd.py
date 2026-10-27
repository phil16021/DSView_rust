import sigrokdecode as srd

class Decoder(srd.Decoder):
    api_version = 3
    id = 'keep_pins'
    name = 'Keep pins'
    longname = 'Keep the wait() result'
    desc = 'Test decoder that holds on to the tuple wait() returned.'
    license = 'gplv2+'
    inputs = ['logic']
    outputs = []
    tags = ['Util']
    channels = (
        {'id': 'data', 'name': 'Data', 'desc': 'Data line', 'idn': 'dec_keep_pins_chan_data'},
    )
    annotations = (
        ('edge', 'Edge with the level before and after'),
    )

    def reset(self):
        pass

    def start(self):
        self.out_ann = self.register(srd.OUTPUT_ANN)

    def decode(self):
        prev = self.wait({0: 'e'})
        while True:
            pins = self.wait({0: 'e'})
            self.put(self.samplenum, self.samplenum, self.out_ann,
                     [0, ['%d->%d' % (prev[0], pins[0])]])
            prev = pins
