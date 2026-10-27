'''
Test decoder: a wait() whose 'skip' count is negative counts as skip 0
and returns normally, also when the type attribute cache misses (F14, F17).
'''

from .pd import Decoder
